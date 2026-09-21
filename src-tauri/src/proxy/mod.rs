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

/// 友盟上报域名关键词（命中即计数并必须放行，最高优先级）
const UMENG_HOST_KEYWORDS: &[&str] = &["umeng", "umengcloud", "alogus", "ulogs", "ucdl.pp.uc.cn"];

/// 友盟官方上报核心域名特征（必须 100% 放行）
pub fn is_umeng_host(host: &str) -> bool {
    UMENG_HOST_KEYWORDS.iter().any(|k| host.contains(k))
}

/// 单个连接允许的最大下行流量（字节）：2.5MB（2,621,440 bytes）
/// 友盟打点仅 2KB~10KB，App 基础 API JSON 仅十几 KB。超过 2.5MB 必为视频流或超大文件预加载，触发物理熔断！
pub const MAX_CONN_DOWNSTREAM_BYTES: u64 = 2_621_440;

/// 格式化字节数为易读字符串（B, KB, MB）
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// 非必要背景大流量黑名单（Google GMS 升级、系统更新、短视频/直播流媒体 CDN、广告预加载等）
pub fn is_blocked_host(host: &str) -> bool {
    // 友盟上报拥有最高优先级，绝对 100% 放行
    if is_umeng_host(host) {
        return false;
    }

    const BLOCKED_SUFFIXES: &[&str] = &[
        // 1. Google / Android 系统与 Play 服务（巨耗流量，占模拟器冷启动大部分无用流量）
        "googleapis.com",
        "gstatic.com",
        "google.com",
        "gvt1.com",
        "gvt2.com",
        "gvt3.com",
        "android.com",
        "ggpht.com",
        "googleusercontent.com",
        "doubleclick.net",
        "1e100.net",
        "app-measurement.com",
        "crashlytics.com",
        "connectivitycheck.android.com",

        // 2. 商业广告联盟与预加载 CDN（穿山甲、优量汇、快手、百度、阿里等）
        "pangle.io",
        "pangolin-sdk.com",
        "pglstatp-toutiao.com",
        "pangolin.snssdk.com",
        "toblog.ctobsnssdk.com",
        "gdt.qq.com",
        "pgdt.gtimg.cn",
        "adkwai.com",
        "kskwai.com",
        "kspkg.com",
        "mobads.baidu.com",
        "mobads-logs.baidu.com",
        "tanx.com",
        "alimama.com",
        "sigmob.cn",
        "beizi.biz",

        // 3. 短视频、流媒体点播与大图 CDN（晨视频及各类媒体资讯类 App 的视频预加载流量大户）
        "snssdk.com",
        "pstatp.com",
        "bytegoofy.com",
        "bytedance.com",
        "ibytedtos.com",
        "toutiaoimg.com",
        "zijieapi.com",
        "kwaicdn.com",
        "ksapisrv.com",
        "yximgs.com",
        "vod.myqcloud.com",
        "live.myqcloud.com",
        "tc-tct.myqcloud.com",
        "vvod.gtimg.com",
        "qcloudcdn.com",
        "alivdn.net",
        "clouddn.com",
        "qiniucdn.com",

        // 4. 第三方推送、崩溃上报、长连接与行为追踪（晨视频内置的辅助非必要 SDK）
        "tpns.tencent.com",
        "api.tpns.tencent.com",
        "bugly.qq.com",
        "android.bugly.qq.com",
        "api-mipush.xiaomi.com",
        "jiguang.cn",
        "jpush.cn",
        "jgapi.cn",
        "talkingdata.net",
        "sensorsdata.cn",
    ];

    for &suffix in BLOCKED_SUFFIXES {
        if host == suffix || host.ends_with(&format!(".{}", suffix)) {
            return true;
        }
    }

    false
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProxyStats {
    /// host -> CONNECT 次数
    pub hosts: HashMap<String, u32>,
    /// host -> 累计传输字节数（包含上传和下载）
    #[serde(default)]
    pub host_bytes: HashMap<String, u64>,
    /// 友盟域名累计命中数
    pub umeng_hits: u32,
    /// 友盟域名累计消耗字节数
    #[serde(default)]
    pub umeng_bytes: u64,
    /// 拦截的非必要背景/广告连接数
    #[serde(default)]
    pub blocked_connects: u32,
    /// 拦截的 host -> 次数
    #[serde(default)]
    pub blocked_hosts: HashMap<String, u32>,
    /// 触发单连接熔断（超 2.5MB）的连接数
    #[serde(default)]
    pub circuit_broken_connects: u32,
    /// 总 CONNECT 数
    pub total_connects: u32,
    /// 累计总消耗字节数（仅放行的实际网络流量）
    #[serde(default)]
    pub total_bytes: u64,
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

/// 从 TLS ClientHello 握手报文中提取 SNI (Server Name Indication)
/// 纯明文 RFC 6066 解析，无需 CA 证书与解密，耗时 < 1 微秒
pub fn parse_tls_sni(data: &[u8]) -> Option<String> {
    // byte 0 必须为 0x16 (Handshake), byte 5 为 0x01 (ClientHello)
    if data.len() < 44 || data[0] != 0x16 || data.get(5) != Some(&0x01) {
        return None;
    }
    // 跳过 TLS Record Header(5) + Handshake Type(1) + Length(3) + Version(2) + Random(32) = 43 字节
    // 第 43 字节是 Session ID 长度
    let session_id_len = *data.get(43)? as usize;
    let mut offset = 44 + session_id_len;

    // Cipher Suites 长度 (2 字节)
    if offset + 2 > data.len() { return None; }
    let cipher_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
    offset += 2 + cipher_len;

    // Compression Methods 长度 (1 字节)
    if offset + 1 > data.len() { return None; }
    let comp_len = *data.get(offset)? as usize;
    offset += 1 + comp_len;

    // Extensions 总长度 (2 字节)
    if offset + 2 > data.len() { return None; }
    let _ext_total_len = u16::from_be_bytes([data[offset], data[offset + 1]]) as usize;
    offset += 2;

    // 遍历所有 Extensions 查找 SNI (extension_type == 0x0000)
    while offset + 4 <= data.len() {
        let ext_type = u16::from_be_bytes([data[offset], data[offset + 1]]);
        let ext_len = u16::from_be_bytes([data[offset + 2], data[offset + 3]]) as usize;
        offset += 4;
        if ext_type == 0 {
            // SNI 结构：ServerNameList 长度 (2 字节) + ServerNameType (1 字节，0=host_name) + NameLen (2 字节) + Name
            if offset + 5 <= data.len() {
                let name_len = u16::from_be_bytes([data[offset + 3], data[offset + 4]]) as usize;
                let name_start = offset + 5;
                if name_start + name_len <= data.len() {
                    return String::from_utf8(data[name_start..name_start + name_len].to_vec()).ok();
                }
            }
            break;
        }
        offset += ext_len;
    }
    None
}

async fn handle_conn(mut client: TcpStream, stats: Arc<RwLock<ProxyStats>>) -> std::io::Result<()> {
    // BufReader 包 &mut 引用，读完头部后交还完整 stream 给 copy
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

    // BufReader 里可能已预读的 body 字节先取出来，随后结束对 client 的借用
    let buffered = reader.buffer().to_vec();
    drop(reader);

    // 统计与过滤
    let host = host_port.split(':').next().unwrap_or("").to_lowercase();
    let is_umeng_target = is_umeng_host(&host);
    let blocked_target = is_blocked_host(&host);

    // 关键优化 1：若 CONNECT 明文目标命中黑名单，直接 403 阻断
    if blocked_target {
        {
            let mut s = stats.write().await;
            *s.hosts.entry(host.clone()).or_insert(0) += 1;
            s.total_connects += 1;
            s.blocked_connects += 1;
            *s.blocked_hosts.entry(host.clone()).or_insert(0) += 1;
        }
        tracing::info!(host = %host, "🛡️ [Proxy 拦截] 已阻断非必要大流量/广告/视频连接，节省手机流量");
        client.write_all(b"HTTP/1.1 403 Forbidden\r\nConnection: close\r\nContent-Length: 0\r\n\r\n").await?;
        return Ok(());
    }

    // 回送 200 Connection Established 开启隧道
    client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await?;

    // 关键优化 2：读取 ClientHello 首包，进行 TLS SNI 毫秒级防漏审计
    // 即使客户端直接用纯 IP（如 58.254.137.162）发起 CONNECT，其 TLS 握手包中仍包含真实域名！
    use tokio::io::AsyncReadExt;
    let mut initial_payload = buffered;
    if initial_payload.is_empty() {
        let mut buf = [0u8; 4096];
        let n = client.read(&mut buf).await.unwrap_or(0);
        if n > 0 {
            initial_payload.extend_from_slice(&buf[..n]);
        }
    }

    let detected_sni = parse_tls_sni(&initial_payload).map(|s| s.to_lowercase());
    let effective_host = detected_sni.as_deref().unwrap_or(&host);
    let is_umeng = is_umeng_target || is_umeng_host(effective_host);
    let blocked_by_sni = !is_umeng && is_blocked_host(effective_host);

    // 统计记录
    {
        let mut s = stats.write().await;
        *s.hosts.entry(effective_host.to_string()).or_insert(0) += 1;
        s.total_connects += 1;
        if is_umeng {
            s.umeng_hits += 1;
        }
        if blocked_by_sni {
            s.blocked_connects += 1;
            *s.blocked_hosts.entry(effective_host.to_string()).or_insert(0) += 1;
        }
    }

    // 关键截杀：若通过 SNI 查出是 Google 更新、广告素材或流媒体 CDN，立即挂断连接，不产生任何下行流量！
    if blocked_by_sni {
        tracing::info!(
            target_ip = %host,
            sni = %effective_host,
            "🛡️ [Proxy SNI 深度阻断] 成功识别纯 IP 伪装，已在握手阶段切断连接，下行流量节省: 0 B"
        );
        let _ = client.shutdown().await;
        return Ok(());
    }

    // 透传合法请求（友盟打点或 App 自身后端业务 API）
    let mut upstream = TcpStream::connect(&host_port).await?;
    if !initial_payload.is_empty() {
        upstream.write_all(&initial_payload).await?;
    }

    // 双向传输与流量精细化统计 + 单连接 2.5MB 熔断器保护
    let (mut client_read, mut client_write) = client.into_split();
    let (mut upstream_read, mut upstream_write) = upstream.into_split();

    // 上行任务：client -> upstream
    let up_task = async move {
        tokio::io::copy(&mut client_read, &mut upstream_write).await
    };

    // 下行任务：upstream -> client（带 2.5MB 物理熔断限额）
    let down_task = async move {
        use tokio::io::AsyncReadExt;
        let mut limited_reader = (&mut upstream_read).take(MAX_CONN_DOWNSTREAM_BYTES + 1);
        let n = tokio::io::copy(&mut limited_reader, &mut client_write).await?;
        let is_exceeded = n > MAX_CONN_DOWNSTREAM_BYTES;
        Ok::<(u64, bool), std::io::Error>((n, is_exceeded))
    };

    let (up_res, down_res) = tokio::join!(up_task, down_task);
    let tx_bytes = up_res.unwrap_or(0);
    let (rx_bytes, circuit_broken) = down_res.unwrap_or((0, false));
    let total_conn_bytes = tx_bytes + rx_bytes;

    // 记录流量字节统计
    {
        let mut s = stats.write().await;
        s.total_bytes += total_conn_bytes;
        *s.host_bytes.entry(host.clone()).or_insert(0) += total_conn_bytes;
        if is_umeng {
            s.umeng_bytes += total_conn_bytes;
        }
        if circuit_broken {
            s.circuit_broken_connects += 1;
        }
    }

    // 打印流量监控日志
    let status_tag = if circuit_broken {
        "⚠️ 触发2.5MB流量熔断切断"
    } else if is_umeng {
        "✅ 友盟打点放行"
    } else {
        "🌐 App业务放行"
    };

    tracing::info!(
        host = %host,
        rx = %format_bytes(rx_bytes),
        tx = %format_bytes(tx_bytes),
        total = %format_bytes(total_conn_bytes),
        tag = %status_tag,
        "[Proxy Traffic] 流量消耗记录"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_host_filtering() {
        // 友盟核心域名必须 100% 放行
        assert!(!is_blocked_host("alogus.umeng.com"));
        assert!(!is_blocked_host("ulogs.umeng.com"));
        assert!(!is_blocked_host("umeng.com"));
        assert!(!is_blocked_host("track.umengcloud.com"));
        assert!(!is_blocked_host("gateway.ucdl.pp.uc.cn"));

        // 普通业务 API 必须放行
        assert!(!is_blocked_host("api.mycompany.com"));
        assert!(!is_blocked_host("login.sampleapp.cn"));
        assert!(!is_blocked_host("app.xxcb.cn"));
        assert!(!is_blocked_host("api.chenshipin.com"));

        // Google / GMS 系统大流量必须拦截
        assert!(is_blocked_host("play.googleapis.com"));
        assert!(is_blocked_host("android.clients.google.com"));
        assert!(is_blocked_host("connectivitycheck.gstatic.com"));
        assert!(is_blocked_host("update.googleapis.com"));
        assert!(is_blocked_host("crashlytics.com"));

        // 第三方广告 / 视频预加载必须拦截
        assert!(is_blocked_host("pangolin-sdk.com"));
        assert!(is_blocked_host("gdt.qq.com"));
        assert!(is_blocked_host("mobads.baidu.com"));
        assert!(is_blocked_host("adkwai.com"));
        assert!(is_blocked_host("sf3-fe-tos.pglstatp-toutiao.com"));

        // 视频流与大文件 CDN 必须拦截
        assert!(is_blocked_host("vod.myqcloud.com"));
        assert!(is_blocked_host("live.myqcloud.com"));
        assert!(is_blocked_host("vvod.gtimg.com"));
        assert!(is_blocked_host("snssdk.com"));
        assert!(is_blocked_host("kwaicdn.com"));

        // 第三方推送与崩溃分析必须拦截
        assert!(is_blocked_host("bugly.qq.com"));
        assert!(is_blocked_host("tpns.tencent.com"));
        assert!(is_blocked_host("api-mipush.xiaomi.com"));
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.00 MB");
    }

    #[test]
    fn test_parse_tls_sni() {
        // 构建一个合成的带 SNI 的 ClientHello 报文（host = "play.googleapis.com"）
        let sni_str = b"play.googleapis.com";
        let mut sni_ext = Vec::new();
        sni_ext.extend_from_slice(&0x0000u16.to_be_bytes()); // ext_type = SNI
        let sni_data_len = (sni_str.len() + 5) as u16;
        sni_ext.extend_from_slice(&sni_data_len.to_be_bytes()); // ext_len
        sni_ext.extend_from_slice(&((sni_str.len() + 3) as u16).to_be_bytes()); // list len
        sni_ext.push(0x00); // host_name type
        sni_ext.extend_from_slice(&(sni_str.len() as u16).to_be_bytes()); // name len
        sni_ext.extend_from_slice(sni_str);

        let mut client_hello = Vec::new();
        client_hello.push(0x16); // Handshake
        client_hello.extend_from_slice(&[0x03, 0x01]); // TLS 1.0
        client_hello.extend_from_slice(&[0x00, 0x00]); // Length placeholder
        client_hello.push(0x01); // ClientHello
        client_hello.extend_from_slice(&[0x00, 0x00, 0x00]); // Handshake len placeholder
        client_hello.extend_from_slice(&[0x03, 0x03]); // Version 1.2
        client_hello.extend_from_slice(&[0u8; 32]); // Random (32 bytes)
        client_hello.push(0x00); // Session ID len = 0
        client_hello.extend_from_slice(&2u16.to_be_bytes()); // Cipher suites len = 2
        client_hello.extend_from_slice(&[0x13, 0x01]); // Cipher suite
        client_hello.push(0x01); // Compression len = 1
        client_hello.push(0x00); // Compression method null
        client_hello.extend_from_slice(&(sni_ext.len() as u16).to_be_bytes()); // Extensions len
        client_hello.extend_from_slice(&sni_ext);

        let parsed = parse_tls_sni(&client_hello);
        assert_eq!(parsed.as_deref(), Some("play.googleapis.com"));
        assert!(is_blocked_host(parsed.as_deref().unwrap()));

        // 非 TLS 数据应返回 None
        assert_eq!(parse_tls_sni(b"GET / HTTP/1.1\r\n\r\n"), None);
        assert_eq!(parse_tls_sni(&[]), None);
    }
}
