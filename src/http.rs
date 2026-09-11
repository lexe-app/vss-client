use prost::bytes::{Bytes, BytesMut};
use std::fmt;

pub(crate) fn new_client(capacity: usize) -> reqwest::Client {
	reqwest::Client::builder()
		.pool_max_idle_per_host(capacity)
		.build()
		.expect("Failed to build HTTP client")
}

#[derive(Debug)]
pub(crate) enum ReadBodyError {
	Request(reqwest::Error),
	TooLarge { limit: usize },
}

impl fmt::Display for ReadBodyError {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::Request(error) => error.fmt(f),
			Self::TooLarge { limit } => write!(f, "HTTP response body exceeds {} bytes", limit),
		}
	}
}

pub(crate) async fn read_body(
	mut response: reqwest::Response, limit: usize,
) -> Result<Bytes, ReadBodyError> {
	let capacity = match response.content_length() {
		Some(length) if length > limit as u64 => return Err(ReadBodyError::TooLarge { limit }),
		Some(length) => length as usize,
		None => 0,
	};
	let mut first_chunk = Bytes::new();
	let mut body = BytesMut::new();
	while let Some(chunk) = response.chunk().await.map_err(ReadBodyError::Request)? {
		let length = first_chunk.len() + body.len();
		if chunk.len() > limit - length {
			return Err(ReadBodyError::TooLarge { limit });
		}
		if chunk.is_empty() {
			continue;
		}
		if length == 0 {
			first_chunk = chunk;
			continue;
		}
		if body.is_empty() {
			body = BytesMut::with_capacity(capacity.max(length + chunk.len()));
			body.extend_from_slice(&first_chunk);
			first_chunk = Bytes::new();
		}
		body.extend_from_slice(&chunk);
	}
	Ok(if body.is_empty() { first_chunk } else { body.freeze() })
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::time::Duration;

	#[tokio::test]
	async fn test_response_body_limit() {
		let client = new_client(1);
		for chunked in [false, true] {
			for length in [0, 4, 5] {
				let body = vec![b'x'; length];
				let mut mock = mockito::mock("GET", "/body-limit").expect(1);
				if chunked {
					let body = body.clone();
					mock = mock.with_body_from_fn(move |writer| {
						for chunk in body.chunks(2) {
							writer.write_all(chunk)?;
						}
						Ok(())
					});
				} else {
					mock = mock.with_body(&body);
				}
				let mock = mock.create();
				let response = client
					.get(format!("{}/body-limit", mockito::server_url()))
					.send()
					.await
					.unwrap();
				assert_eq!(
					response.content_length(),
					if chunked { None } else { Some(length as u64) }
				);
				let result = read_body(response, 4).await;
				if length <= 4 {
					assert_eq!(result.unwrap(), body);
				} else {
					assert!(matches!(result, Err(ReadBodyError::TooLarge { limit: 4 })));
				}
				mock.assert();
			}
		}
	}

	#[tokio::test]
	async fn test_timeout_covers_response_body() {
		let mock = mockito::mock("GET", "/slow-body")
			.with_body_from_fn(|writer| {
				writer.write_all(b"first chunk")?;
				std::thread::sleep(Duration::from_millis(200));
				writer.write_all(b"last chunk")
			})
			.expect(1)
			.create();
		let client = reqwest::Client::builder().timeout(Duration::from_millis(50)).build().unwrap();
		let response =
			client.get(format!("{}/slow-body", mockito::server_url())).send().await.unwrap();
		assert!(matches!(
			read_body(response, 100).await,
			Err(ReadBodyError::Request(error)) if error.is_timeout()
		));
		mock.assert();
	}
}
