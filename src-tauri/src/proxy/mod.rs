//! 内嵌轻量 TCP 代理（设计文档 §7.2a，v1.1 P1-2）
//! 只做一件事：解析明文 CONNECT <host>:443，按 host 统计次数，然后透传。
//! 不解密 HTTPS、不装 CA 证书、不碰 network_security_config。
//! 「App 确实尝试向友盟服务器发了 N 次请求」= 友盟域名 CONNECT 命中数——GATE 1 第一问的硬证据。

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

/// 友盟上报域名关键词（命中即计数）
const UMENG_HOST_KEYWORDS: &[&str] = &["umeng", "umengcloud", "alogus", "ulogs"];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyStats {
    /// host -> CONNECT 次数
    pub hosts: HashMap<String, u32>,
    /// 友盟域名累计命中数
    pub umeng_hits: u32,
    /// 总 CONNECT 数
    pub total_connects: u32,
}

pub struct CountingProxy {
    pub port: u16,
    pub stats: Arc<RwLock<ProxyStats>>,
    pub shutdown: tokio_util::sync::CancellationToken,
}

impl CountingProxy {
    /// 在随机空闲端口启动代理
    pub async fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        let stats = Arc::new(RwLock::new(ProxyStats::default()));
        let shutdown = tokio_util::sync::CancellationToken::new();

        let stats_clone = stats.clone();
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = shutdown_clone.cancelled() => break,
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, _)) => {
                                let stats = stats_clone.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_conn(stream, stats).await {
                                        tracing::debug!("proxy conn error: {}", e);
                                    }
                                });
                            }
                            Err(e) => {
                                tracing::warn!("proxy accept error: {}", e);
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            }
                        }
                    }
                }
            }
        });

        Ok(Self { port, stats, shutdown })
    }

    pub async fn snapshot(&self) -> ProxyStats {
        self.stats.read().await.clone()
    }

    pub async fn reset(&self) {
        *self.stats.write().await = ProxyStats::default();
    }

    pub fn stop(&self) {
        self.shutdown.cancel();
    }
}

async fn handle_conn(mut client: TcpStream, stats: Arc<RwLock<ProxyStats>>) -> std::io::Result<()> {
    // BufReader 包 &mut 引用，读完头部后交还完整 stream 给 copy_bidirectional
    let mut reader = BufReader::new(&mut client);

    // 读取请求行：CONNECT host:port HTTP/1.1
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    let parts: Vec<&str> = request_line.split_whitespace().collect();
    if parts.len() < 2 || parts[0] != "CONNECT" {
        // 非 CONNECT 请求（明文 HTTP），简单回 405
        client.write_all(b"HTTP/1.1 405 Method Not Allowed\r\n\r\n").await?;
        return Ok(());
    }
    let host_port = parts[1].to_string();

    // 丢弃剩余 header
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).await?;
        if line.trim().is_empty() {
            break;
        }
    }

    // 统计
    let host = host_port.split(':').next().unwrap_or("").to_lowercase();
    {
        let mut s = stats.write().await;
        *s.hosts.entry(host.clone()).or_insert(0) += 1;
        s.total_connects += 1;
        if UMENG_HOST_KEYWORDS.iter().any(|k| host.contains(k)) {
            s.umeng_hits += 1;
        }
    }

    // BufReader 里可能已预读的 body 字节先取出来，随后结束对 client 的借用
    let buffered = reader.buffer().to_vec();
    drop(reader);

    // 透传
    let mut upstream = TcpStream::connect(&host_port).await?;
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;
    if !buffered.is_empty() {
        upstream.write_all(&buffered).await?;
    }

    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}
