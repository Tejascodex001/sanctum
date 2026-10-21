//! Delivering feedback (see `sanctum_core::feedback`): one connection per
//! piece of feedback, to the developer's feedback onion service.

use crate::engine::BoxConn;
use sanctum_core::feedback::{decode, encode, Feedback, MAX_WIRE, RECEIVED};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::timeout;

/// Each side gets this long to say its part.
const PATIENCE: Duration = Duration::from_secs(60);
/// The developer's phone takes at most this many pieces of feedback...
const GATE_COUNT: usize = 30;
/// ...in this long, from everyone together (senders are anonymous).
const GATE_WINDOW: Duration = Duration::from_secs(3600);

/// Sends `feedback` over `conn` and waits until the other side has it.
pub async fn send(conn: &mut BoxConn, feedback: &Feedback) -> anyhow::Result<()> {
    timeout(PATIENCE, async {
        conn.write_all(&encode(feedback)).await?;
        conn.flush().await?;
        let mut answer = [0u8; 2];
        conn.read_exact(&mut answer).await?;
        anyhow::ensure!(answer == *RECEIVED, "not received");
        Ok(())
    })
    .await?
}

/// Reads one piece of feedback from `conn`, hands it to `keep` (which says
/// whether it was kept) and, if so, says it's received.
pub async fn serve(mut conn: BoxConn, gate: &Gate, keep: impl FnOnce(Feedback) -> bool) -> anyhow::Result<()> {
    let feedback = timeout(PATIENCE, async {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match decode(&buf) {
                Ok(Some(feedback)) => return Ok(feedback),
                Ok(None) => {}
                Err(_) => anyhow::bail!("not feedback"),
            }
            let n = conn.read(&mut chunk).await?;
            anyhow::ensure!(n > 0 && buf.len() + n <= MAX_WIRE, "closed or too big");
            buf.extend_from_slice(&chunk[..n]);
        }
    })
    .await??;
    anyhow::ensure!(gate.pass(), "too much feedback at once");
    anyhow::ensure!(keep(feedback), "not taking feedback");
    timeout(PATIENCE, async {
        conn.write_all(RECEIVED).await?;
        conn.flush().await
    })
    .await??;
    Ok(())
}

/// Lets through at most a few pieces of feedback an hour, so nobody can
/// flood the developer's inbox.
#[derive(Default)]
pub struct Gate {
    passed: Mutex<VecDeque<Instant>>,
}

impl Gate {
    fn pass(&self) -> bool {
        let now = Instant::now();
        let mut passed = self.passed.lock().unwrap();
        while passed.front().is_some_and(|t| now.duration_since(*t) >= GATE_WINDOW) {
            passed.pop_front();
        }
        if passed.len() >= GATE_COUNT {
            return false;
        }
        passed.push_back(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sanctum_core::feedback::MAGIC;
    use std::sync::Arc;
    use tokio::net::{TcpListener, TcpStream};

    fn sample(text: &str) -> Feedback {
        Feedback { phone: "Pixel 8".into(), android: "15".into(), build: 65, rating: 5, text: text.into() }
    }

    /// A developer's phone on loopback: what it kept.
    async fn developer(gate: Arc<Gate>, taking: bool) -> (std::net::SocketAddr, Arc<Mutex<Vec<Feedback>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let kept = Arc::new(Mutex::new(Vec::new()));
        let inbox = kept.clone();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let (gate, inbox) = (gate.clone(), inbox.clone());
                tokio::spawn(async move {
                    let _ = serve(Box::new(stream), &gate, |f| {
                        if taking {
                            inbox.lock().unwrap().push(f);
                        }
                        taking
                    })
                    .await;
                });
            }
        });
        (addr, kept)
    }

    async fn dial(addr: std::net::SocketAddr) -> BoxConn {
        Box::new(TcpStream::connect(addr).await.unwrap())
    }

    #[tokio::test]
    async fn feedback_crosses_a_real_connection() {
        let (addr, kept) = developer(Arc::default(), true).await;
        send(&mut dial(addr).await, &sample("Works great")).await.unwrap();
        assert_eq!(*kept.lock().unwrap(), [sample("Works great")]);
    }

    #[tokio::test]
    async fn the_sender_hears_when_it_isnt_kept() {
        let (addr, kept) = developer(Arc::default(), false).await;
        assert!(send(&mut dial(addr).await, &sample("Hello?")).await.is_err());
        assert!(kept.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn junk_and_floods_are_turned_away() {
        let (addr, kept) = developer(Arc::default(), true).await;
        let mut junk = dial(addr).await;
        junk.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
        let mut answer = Vec::new();
        let _ = junk.read_to_end(&mut answer).await;
        assert!(answer.is_empty());
        let mut huge = dial(addr).await;
        huge.write_all(MAGIC).await.unwrap();
        huge.write_all(&(1u32 << 20).to_be_bytes()).await.unwrap();
        let _ = huge.read_to_end(&mut answer).await;
        assert!(answer.is_empty());
        for i in 0..GATE_COUNT {
            send(&mut dial(addr).await, &sample(&format!("note {i}"))).await.unwrap();
        }
        assert!(send(&mut dial(addr).await, &sample("one too many")).await.is_err());
        assert_eq!(kept.lock().unwrap().len(), GATE_COUNT);
    }
}
