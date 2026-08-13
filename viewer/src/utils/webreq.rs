use ehttp::{Method, Request};

use super::yield_to_ui;

/// How many times a request is made before its failure is the caller's. A name that fails to
/// resolve, a connection refused or a socket that times out is a fault of the moment rather than of
/// the URL, and one that answers at all -- even to say the file is not there -- is not retried.
const ATTEMPTS: usize = 3;

pub struct HttpResponse {
    pub status: u16,
    pub ok: bool,
    pub bytes: Vec<u8>,
    pub headers: ehttp::Headers,
}

impl HttpResponse {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

impl From<ehttp::Response> for HttpResponse {
    fn from(response: ehttp::Response) -> Self {
        Self {
            status: response.status,
            ok: response.ok,
            bytes: response.bytes,
            headers: response.headers,
        }
    }
}

pub async fn request(
    method: &str,
    url: impl ToString,
    headers: &[(&str, &str)],
    body: Option<Vec<u8>>,
) -> anyhow::Result<HttpResponse> {
    let mut req = Request::get(url);
    req.method = Method::parse(method).map_err(|e| anyhow::anyhow!("invalid HTTP method: {e}"))?;
    if let Some(body) = body {
        req.body = body;
    }
    for (key, value) in headers {
        req.headers.insert(*key, *value);
    }

    Ok(send(req).await?.into())
}

/// Makes the request, and makes it again where the transport rather than the server was what
/// failed.
///
/// The channel is ours rather than [`ehttp::fetch_async`]'s: that one unwraps the send back to the
/// caller, so abandoning a request -- which is what dropping whatever asked for it does -- takes
/// down the thread carrying the answer.
async fn send(request: Request) -> anyhow::Result<ehttp::Response> {
    let mut failed = String::new();
    for attempt in 0..ATTEMPTS {
        if attempt > 0 {
            yield_to_ui().await;
        }
        let (tx, rx) = async_channel::bounded(1);
        ehttp::fetch(request.clone(), move |received| {
            // Nowhere to send it is not a fault: whatever asked has been dropped.
            let _ = tx.try_send(received);
        });
        match rx.recv().await {
            Ok(Ok(response)) => return Ok(response),
            Ok(Err(why)) => failed = why,
            Err(why) => failed = why.to_string(),
        }
    }
    Err(anyhow::anyhow!(failed))
}

pub async fn fetch(url: impl ToString) -> anyhow::Result<HttpResponse> {
    let resp = send(Request::get(url)).await?;

    if !resp.ok {
        anyhow::bail!(
            "Response not OK ({}{}{}): {}",
            resp.status,
            if resp.status_text.is_empty() { "" } else { " " },
            resp.status_text,
            String::from_utf8_lossy(&resp.bytes)
        );
    }

    Ok(resp.into())
}

pub async fn fetch_url(url: impl ToString) -> anyhow::Result<Vec<u8>> {
    Ok(fetch(url).await?.bytes)
}

pub async fn fetch_url_str(url: impl ToString) -> anyhow::Result<String> {
    let bytes = fetch_url(url).await?;
    Ok(String::from_utf8(bytes)?)
}
