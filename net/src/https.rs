//! A small HTTPS client for checking and downloading app updates, and for
//! Reels, through Tor, so sites never see the phone's address. (Reels'
//! opt-in fast mode connects directly instead: see [`direct`].) HTTP/1.1
//! GET only, following redirects; TLS with rustls and the Mozilla root
//! store.

use crate::engine::BoxConn;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_rustls::TlsConnector;

/// Opens a TCP-like stream to a host (through Tor in the app).
pub type Connect = Arc<dyn Fn(String, u16) -> Pin<Box<dyn Future<Output = anyhow::Result<BoxConn>> + Send>> + Send + Sync>;

const MAX_REDIRECTS: usize = 5;
const MAX_HEADERS: usize = 64 * 1024;

/// GETs `url` (https only), following redirects, reading at most `limit`
/// bytes of body. `progress` hears (received, total if known).
pub async fn get(connect: &Connect, url: &str, limit: usize, progress: &(dyn Fn(u64, Option<u64>) + Sync)) -> anyhow::Result<Vec<u8>> {
    get_trusting(connect, url, limit, progress, mozilla_roots()).await
}

/// Where `url` redirects to, without following it. (E.g. a GitHub
/// project's `releases/latest` points at the newest release, no API or
/// rate limit involved.)
pub async fn redirect_of(connect: &Connect, url: &str) -> anyhow::Result<String> {
    redirect_trusting(connect, url, mozilla_roots()).await
}

/// Wraps `connect` so it opens TLS connections (checked against the
/// Mozilla roots) instead of raw ones.
pub fn tls(connect: Connect) -> Connect {
    Arc::new(move |host: String, port: u16| {
        let connect = connect.clone();
        Box::pin(async move {
            crate::tor::install_crypto();
            let config = ClientConfig::builder().with_root_certificates(mozilla_roots()).with_no_client_auth();
            let tcp = connect(host.clone(), port).await?;
            let stream = TlsConnector::from(Arc::new(config)).connect(ServerName::try_from(host)?, tcp).await?;
            Ok(Box::new(stream) as BoxConn)
        })
    })
}

/// Opens plain TCP straight to a host, without Tor, so the site sees this
/// phone's IP address. Only for Reels' opt-in fast mode: friends and
/// updates always go through Tor. (TLS is added on top as usual.) Hosts
/// inside the phone or on the local network are refused, so a post can't
/// point the app at the user's router.
pub fn direct() -> Connect {
    Arc::new(|host: String, port: u16| {
        Box::pin(async move {
            let open = async {
                let public: Vec<SocketAddr> =
                    tokio::net::lookup_host((host.as_str(), port)).await?.filter(|a| is_public(&a.ip())).collect();
                anyhow::ensure!(!public.is_empty(), "{host} isn't on the public internet");
                race(&public).await
            };
            let stream = tokio::time::timeout(DIRECT_TIMEOUT, open).await??;
            Ok(Box::new(stream) as BoxConn)
        })
    })
}

const DIRECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
/// Head start each address gets before the next is tried too.
const NEXT_AFTER: std::time::Duration = std::time::Duration::from_millis(300);

/// Connects to whichever address answers first, trying them in turn but
/// not waiting for one to fail first ("happy eyeballs"). Many mobile
/// networks hand out IPv6 addresses that go nowhere: trying them one by one
/// could hang for the whole timeout before IPv4 is tried.
async fn race(addrs: &[SocketAddr]) -> anyhow::Result<tokio::net::TcpStream> {
    // Alternate IPv6 and IPv4, so one family can't hold up the other.
    let (v6, v4): (Vec<SocketAddr>, Vec<SocketAddr>) = addrs.iter().partition(|a| a.is_ipv6());
    let mut order: Vec<SocketAddr> = Vec::new();
    for i in 0..v6.len().max(v4.len()) {
        order.extend(v6.get(i).copied());
        order.extend(v4.get(i).copied());
    }
    let mut tries = tokio::task::JoinSet::new();
    let mut last = anyhow::anyhow!("no address answered");
    let mut queue = order.into_iter();
    loop {
        if let Some(addr) = queue.next() {
            tries.spawn(async move { tokio::net::TcpStream::connect(addr).await });
        } else if tries.is_empty() {
            return Err(last);
        }
        let next_due = tokio::time::sleep(NEXT_AFTER);
        tokio::select! {
            done = tries.join_next(), if !tries.is_empty() => match done {
                Some(Ok(Ok(stream))) => return Ok(stream),
                Some(Ok(Err(e))) => last = e.into(),
                Some(Err(e)) => last = e.into(),
                None => {}
            },
            // Nothing yet: start the next address alongside.
            _ = next_due, if queue.len() > 0 => {}
        }
    }
}

/// Somewhere on the internet, not this phone or the network it's on.
fn is_public(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_multicast()
                || a == 0
                // Carrier-grade NAT (100.64.0.0/10).
                || (a == 100 && (b & 0xc0) == 64))
        }
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_public(&IpAddr::V4(v4)),
            None => {
                let first = v6.segments()[0];
                !(v6.is_loopback()
                    || v6.is_unspecified()
                    || v6.is_multicast()
                    // Unique local (fc00::/7) and link-local (fe80::/10).
                    || (first & 0xfe00) == 0xfc00
                    || (first & 0xffc0) == 0xfe80)
            }
        },
    }
}

fn mozilla_roots() -> RootCertStore {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    roots
}

async fn redirect_trusting(connect: &Connect, url: &str, roots: RootCertStore) -> anyhow::Result<String> {
    match fetch(connect, url, 1 << 20, &|_, _| {}, roots, false).await? {
        Fetched::Redirect(to) => Ok(to),
        Fetched::Body(_) => anyhow::bail!("{url} didn't redirect"),
    }
}

async fn get_trusting(
    connect: &Connect,
    url: &str,
    limit: usize,
    progress: &(dyn Fn(u64, Option<u64>) + Sync),
    roots: RootCertStore,
) -> anyhow::Result<Vec<u8>> {
    match fetch(connect, url, limit, progress, roots, true).await? {
        Fetched::Body(body) => Ok(body),
        Fetched::Redirect(_) => anyhow::bail!("too many redirects"),
    }
}

enum Fetched {
    Body(Vec<u8>),
    Redirect(String),
}

async fn fetch(
    connect: &Connect,
    url: &str,
    limit: usize,
    progress: &(dyn Fn(u64, Option<u64>) + Sync),
    roots: RootCertStore,
    follow: bool,
) -> anyhow::Result<Fetched> {
    crate::tor::install_crypto();
    let tls = TlsConnector::from(Arc::new(ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()));

    let mut url = url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        let (host, port, path) = parse_url(&url)?;
        let tcp = connect(host.clone(), port).await?;
        let mut stream = tls.connect(ServerName::try_from(host.clone())?, tcp).await?;
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {host}\r\nUser-Agent: Sanctum\r\nAccept: */*\r\nAccept-Encoding: identity\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await?;
        stream.flush().await?;

        // Headers.
        let mut buf = Vec::new();
        let mut chunk = [0u8; 16 * 1024];
        let head_end = loop {
            if let Some(i) = find(&buf, b"\r\n\r\n") {
                break i + 4;
            }
            anyhow::ensure!(buf.len() < MAX_HEADERS, "response headers too long");
            let n = stream.read(&mut chunk).await?;
            anyhow::ensure!(n > 0, "connection closed before the response");
            buf.extend_from_slice(&chunk[..n]);
        };
        let head = Head::parse(&buf[..head_end])?;
        let mut body = buf[head_end..].to_vec();

        if (300..400).contains(&head.status) {
            let location = head.header("location").ok_or_else(|| anyhow::anyhow!("redirect without a location"))?;
            url = if location.starts_with("https://") {
                location.to_string()
            } else {
                format!("https://{host}{location}")
            };
            if !follow {
                return Ok(Fetched::Redirect(url));
            }
            continue;
        }
        if head.status != 200 {
            let n = stream.read(&mut chunk).await.unwrap_or(0);
            body.extend_from_slice(&chunk[..n]);
            let said = String::from_utf8_lossy(&body[..body.len().min(160)]).into_owned();
            anyhow::bail!("HTTP {} from {host}: {}", head.status, said.trim());
        }

        let total = head.header("content-length").and_then(|v| v.parse::<u64>().ok());
        if let Some(total) = total {
            anyhow::ensure!(total as usize <= limit, "too large ({total} bytes)");
        }
        // Body: to the length given, or to the end of the connection.
        loop {
            progress(body.len() as u64, total);
            if total.is_some_and(|t| body.len() as u64 >= t) {
                break;
            }
            let n = stream.read(&mut chunk).await.or_else(|e| {
                // Servers often close TLS without a close_notify after the
                // body; with a known length that's harmless.
                if e.kind() == std::io::ErrorKind::UnexpectedEof { Ok(0) } else { Err(e) }
            })?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&chunk[..n]);
            anyhow::ensure!(body.len() <= limit, "too large");
        }
        if let Some(total) = total {
            anyhow::ensure!(body.len() as u64 >= total, "download cut off");
            body.truncate(total as usize);
        }
        if head.header("transfer-encoding").is_some_and(|v| v.eq_ignore_ascii_case("chunked")) {
            body = dechunk(&body)?;
        }
        return Ok(Fetched::Body(body));
    }
    anyhow::bail!("too many redirects")
}

struct Head {
    status: u16,
    headers: Vec<(String, String)>,
}

impl Head {
    fn parse(bytes: &[u8]) -> anyhow::Result<Self> {
        let text = std::str::from_utf8(bytes)?;
        let mut lines = text.split("\r\n");
        let status_line = lines.next().unwrap_or_default();
        let status = status_line
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("bad status line: {status_line}"))?;
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        Ok(Self { status, headers })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

fn parse_url(url: &str) -> anyhow::Result<(String, u16, String)> {
    let rest = url.strip_prefix("https://").ok_or_else(|| anyhow::anyhow!("not https: {url}"))?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse()?),
        None => (authority, 443),
    };
    anyhow::ensure!(!host.is_empty(), "no host in {url}");
    Ok((host.to_string(), port, path.to_string()))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Undoes chunked transfer encoding.
fn dechunk(mut data: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::new();
    loop {
        let line_end = find(data, b"\r\n").ok_or_else(|| anyhow::anyhow!("bad chunk"))?;
        let size_text = std::str::from_utf8(&data[..line_end])?;
        let size = usize::from_str_radix(size_text.split(';').next().unwrap_or("").trim(), 16)?;
        data = &data[line_end + 2..];
        if size == 0 {
            return Ok(out);
        }
        anyhow::ensure!(data.len() >= size + 2, "chunk cut off");
        out.extend_from_slice(&data[..size]);
        data = &data[size + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn racing_skips_addresses_that_go_nowhere() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live = listener.local_addr().unwrap();
        // A documentation address (never answers) and a closed port come first.
        let closed = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap()
        };
        let nowhere: SocketAddr = "192.0.2.1:9".parse().unwrap();
        let started = std::time::Instant::now();
        let stream = race(&[nowhere, closed, live]).await.unwrap();
        assert_eq!(stream.peer_addr().unwrap(), live);
        assert!(started.elapsed() < std::time::Duration::from_secs(3), "{:?}", started.elapsed());
        assert!(race(&[closed]).await.is_err());
    }

    #[test]
    fn urls() {
        assert_eq!(
            parse_url("https://api.github.com/repos/a/b/releases/latest").unwrap(),
            ("api.github.com".into(), 443, "/repos/a/b/releases/latest".into())
        );
        assert_eq!(parse_url("https://h:8443").unwrap(), ("h".into(), 8443, "/".into()));
        assert!(parse_url("http://plain.example/").is_err(), "https only");
    }

    #[test]
    fn direct_mode_reaches_only_the_public_internet() {
        for ip in ["93.184.216.34", "2606:2800:220:1::1", "::ffff:93.184.216.34"] {
            assert!(is_public(&ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "127.0.0.1", "10.0.0.1", "192.168.1.1", "172.16.0.1", "169.254.1.1", "100.64.0.1", "0.0.0.0",
            "255.255.255.255", "::1", "fe80::1", "fd00::1", "::ffff:192.168.1.1", "224.0.0.1",
        ] {
            assert!(!is_public(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn direct_mode_refuses_the_phone_itself() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(direct()("127.0.0.1".into(), port).await.is_err());
        assert!(direct()("localhost".into(), port).await.is_err());
    }

    #[test]
    fn heads_and_chunks() {
        let head = Head::parse(b"HTTP/1.1 302 Found\r\nLocation: https://x/y\r\nContent-Length: 0\r\n\r\n").unwrap();
        assert_eq!((head.status, head.header("location")), (302, Some("https://x/y")));
        assert_eq!(dechunk(b"5\r\nhello\r\n6;ext=1\r\n world\r\n0\r\n\r\n").unwrap(), b"hello world");
        assert!(dechunk(b"ff\r\nshort\r\n").is_err());
    }
}

#[cfg(test)]
mod live {
    use super::*;
    use std::sync::Mutex;

    /// Against the real GitHub (plain TCP standing in for Tor). Needs the
    /// network: `cargo test -p sanctum-net live -- --ignored`. Behind a
    /// TLS-inspecting proxy, point EXTRA_CA at its certificate.
    #[tokio::test]
    #[ignore]
    async fn github_release_and_apk() {
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        if let Ok(path) = std::env::var("EXTRA_CA") {
            use tokio_rustls::rustls::pki_types::{pem::PemObject, CertificateDer};
            for cert in CertificateDer::pem_file_iter(path).unwrap() {
                let _ = roots.add(cert.unwrap());
            }
        }
        let connect: Connect = Arc::new(|host, port| {
            Box::pin(async move { Ok(Box::new(tokio::net::TcpStream::connect((host.as_str(), port)).await?) as BoxConn) })
        });
        let latest = redirect_trusting(&connect, "https://github.com/Tejascodex001/sanctum/releases/latest", roots.clone())
            .await
            .unwrap();
        let tag = latest.rsplit('/').next().unwrap();
        assert!(tag.starts_with("build-"), "{latest}");
        let apk_url = format!("https://github.com/Tejascodex001/sanctum/releases/download/{tag}/app-release.apk");
        let apk_url = apk_url.as_str();
        let last = Mutex::new((0, None));
        let apk = get_trusting(&connect, apk_url, 200 << 20, &|got, total| *last.lock().unwrap() = (got, total), roots)
            .await
            .unwrap();
        assert_eq!(&apk[..2], b"PK", "an APK is a zip");
        assert_eq!(*last.lock().unwrap(), (apk.len() as u64, Some(apk.len() as u64)));
    }
}
