//! Plays videos from the internet through Tor.
//!
//! The video player only speaks plain HTTP to a URL. So the app hands it a
//! local one (`http://127.0.0.1:port/<token>`), and this relay forwards
//! each request, ranges included, to the real `https://` address through
//! Tor, following redirects itself so the player never learns of (or
//! connects to) anything outside the phone. It listens on the loopback
//! address only, and answers only for tokens it handed out: random, and
//! each tied to one address.

use crate::engine::BoxConn;
use crate::https::Connect;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

const MAX_HEAD: usize = 16 * 1024;
const MAX_REDIRECTS: usize = 5;
/// Addresses remembered (the oldest are forgotten first).
const MAX_TOKENS: usize = 64;

pub struct Relay {
    port: u16,
    routes: Arc<Mutex<Routes>>,
}

#[derive(Default)]
struct Routes {
    by_token: HashMap<String, String>,
    order: Vec<String>,
}

impl Relay {
    /// Starts listening. `open` reaches an `https://` host: TLS over Tor in
    /// the app (see `https::tls`).
    pub async fn start(open: Connect) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let routes: Arc<Mutex<Routes>> = Default::default();
        let r = routes.clone();
        tokio::spawn(async move {
            while let Ok((client, _)) = listener.accept().await {
                let (open, routes) = (open.clone(), r.clone());
                tokio::spawn(async move {
                    let _ = serve(client, open, routes).await;
                });
            }
        });
        Ok(Self { port, routes })
    }

    /// A local address the player can use for `url` (`https://` only).
    pub fn url_for(&self, url: &str) -> anyhow::Result<String> {
        anyhow::ensure!(url.starts_with("https://"), "not https: {url}");
        let token: String = rand::random::<[u8; 16]>().iter().map(|b| format!("{b:02x}")).collect();
        let mut routes = self.routes.lock().unwrap();
        routes.by_token.insert(token.clone(), url.to_string());
        routes.order.push(token.clone());
        if routes.order.len() > MAX_TOKENS {
            let old = routes.order.remove(0);
            routes.by_token.remove(&old);
        }
        Ok(format!("http://127.0.0.1:{}/{token}", self.port))
    }
}

async fn read_head<S: AsyncReadExt + Unpin>(stream: &mut S) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(i + 4);
            return Ok((buf, rest));
        }
        anyhow::ensure!(buf.len() < MAX_HEAD, "head too long");
        let n = stream.read(&mut chunk).await?;
        anyhow::ensure!(n > 0, "closed");
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn header<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    head.split("\r\n").skip(1).find_map(|line| {
        let (k, v) = line.split_once(':')?;
        k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
    })
}

async fn serve(mut client: TcpStream, open: Connect, routes: Arc<Mutex<Routes>>) -> anyhow::Result<()> {
    let (head, _) = read_head(&mut client).await?;
    let head = String::from_utf8(head)?;
    let mut parts = head.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let url = routes.lock().unwrap().by_token.get(path.trim_start_matches('/')).cloned();
    let (Some(mut url), "GET" | "HEAD") = (url, method) else {
        client.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
        return Ok(());
    };
    let range = header(&head, "range").map(|r| format!("Range: {r}\r\n")).unwrap_or_default();
    for _ in 0..=MAX_REDIRECTS {
        let (host, port, path) = split(&url)?;
        let mut remote: BoxConn = open(host.clone(), port).await?;
        let request = format!(
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Sanctum\r\nAccept: */*\r\n{range}Connection: close\r\n\r\n"
        );
        remote.write_all(request.as_bytes()).await?;
        remote.flush().await?;
        let (reply, rest) = read_head(&mut remote).await?;
        let text = String::from_utf8_lossy(&reply);
        let status: u16 = text.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        if (300..400).contains(&status) {
            if let Some(location) = header(&text, "location") {
                url = if location.starts_with("https://") {
                    location.to_string()
                } else if location.starts_with('/') {
                    format!("https://{host}{location}")
                } else {
                    anyhow::bail!("unsupported redirect")
                };
                continue;
            }
        }
        // Everything else goes back to the player as it came.
        client.write_all(&reply).await?;
        client.write_all(&rest).await?;
        tokio::io::copy(&mut remote, &mut client).await?;
        client.shutdown().await?;
        return Ok(());
    }
    client.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await?;
    Ok(())
}

fn split(url: &str) -> anyhow::Result<(String, u16, String)> {
    let rest = url.strip_prefix("https://").ok_or_else(|| anyhow::anyhow!("not https"))?;
    let (authority, path) = rest.find('/').map_or((rest, "/"), |i| (&rest[..i], &rest[i..]));
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse()?),
        None => (authority, 443),
    };
    Ok((host.to_string(), port, path.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake "internet": plain TCP servers standing in for HTTPS hosts.
    /// `files.example` redirects to `cdn.example`, which serves ranges.
    async fn fake_internet() -> Connect {
        let video: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = listener.accept().await {
                let video = video.clone();
                tokio::spawn(async move {
                    let (head, _) = read_head(&mut s).await.unwrap();
                    let head = String::from_utf8(head).unwrap();
                    let reply = if header(&head, "host") == Some("files.example") {
                        b"HTTP/1.1 302 Found\r\nLocation: https://cdn.example/v.mp4\r\nContent-Length: 0\r\n\r\n".to_vec()
                    } else {
                        let from: usize = header(&head, "range")
                            .and_then(|r| r.strip_prefix("bytes="))
                            .and_then(|r| r.split('-').next())
                            .and_then(|n| n.parse().ok())
                            .unwrap_or(0);
                        let body = &video[from..];
                        let mut r = format!(
                            "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {from}-{}/{}\r\n\r\n",
                            body.len(),
                            video.len() - 1,
                            video.len()
                        )
                        .into_bytes();
                        r.extend_from_slice(body);
                        r
                    };
                    s.write_all(&reply).await.unwrap();
                });
            }
        });
        Arc::new(move |_host, _port| {
            Box::pin(async move { Ok(Box::new(TcpStream::connect(("127.0.0.1", port)).await?) as BoxConn) })
        })
    }

    async fn get(url: &str, range: &str) -> (String, Vec<u8>) {
        let rest = url.strip_prefix("http://").unwrap();
        let (authority, path) = rest.split_at(rest.find('/').unwrap());
        let mut s = TcpStream::connect(authority).await.unwrap();
        s.write_all(format!("GET {path} HTTP/1.1\r\nHost: x\r\nRange: {range}\r\n\r\n").as_bytes()).await.unwrap();
        let mut all = Vec::new();
        s.read_to_end(&mut all).await.unwrap();
        let i = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        (String::from_utf8_lossy(&all[..i]).into_owned(), all[i..].to_vec())
    }

    #[tokio::test]
    async fn relays_ranges_and_follows_redirects_itself() {
        let relay = Relay::start(fake_internet().await).await.unwrap();
        let local = relay.url_for("https://files.example/watch/1").unwrap();
        assert!(local.starts_with("http://127.0.0.1:"));
        let (head, body) = get(&local, "bytes=9000-").await;
        assert!(head.starts_with("HTTP/1.1 206"), "{head}");
        assert_eq!(body.len(), 1000);
        assert_eq!(body[0], (9000 % 251) as u8);
        // Unknown tokens get nothing.
        let (head, _) = get(&local.replace(&local[local.len() - 4..], "beef"), "bytes=0-").await;
        assert!(head.starts_with("HTTP/1.1 404"));
        assert!(relay.url_for("http://plain.example/").is_err());
    }
}
