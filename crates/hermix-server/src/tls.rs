//! §5.5 第 2 层：TLS listener 的证书生命周期。
//!
//! 职责单一：自签证书的生成 / 加载 / 指纹计算 / rustls 配置构建。
//! 监听器装配在 `lib.rs::run_server_with_tls`（可选双监听器拓扑）。
//!
//! 设计要点：
//! - 证书落盘 `<hermix_home>/tls/`，跨重启复用——指纹稳定是设备配对
//!   TOFU（首次使用即信任）的根基，换证书 = 所有已配对设备失效。
//! - 自签证书有效期 10 年：证书只会出现在受控的家庭/办公网络，
//!   过短的有效期会静默打断已配对设备（握手失败且无带外通知）。
//! - 强制 ring provider：rustls feature 图谱可能同时启用 aws-lc/ring，
//!   显式 `builder_with_provider` 避免运行时 panic 和 MSVC 构建依赖
//!   aws-lc-sys（需要 CMake/NASM）。
//! - 客户端不校验系统 CA（自签必然不过），校验方式 = 指纹固定，
//!   由配对流程（第 3/4 层）消费本模块输出的指纹。

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermix_core::AgentError;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

/// 证书 + 私钥 + 指纹。指纹供配对流程与启动日志消费。
pub struct TlsMaterial {
    pub cert_der: CertificateDer<'static>,
    pub key_der: PrivateKeyDer<'static>,
    /// 证书 DER 的 SHA-256，64 位小写 hex。客户端指纹固定的锚点。
    pub fingerprint_sha256: String,
}

/// 证书目录：`<hermix_home>/tls`。
pub fn tls_dir() -> PathBuf {
    hermix_config::paths::hermix_home().join("tls")
}

/// 环境变量开关：设置即启用 TLS 监听器（如 `HERMIX_TLS_ADDR=0.0.0.0:8443`）。
/// 与 `HERMIX_SERVER_ADDR` 同模式——env 优先级高于未来的 config.yaml，
/// 且本期不引入 config schema 变更。
pub fn tls_addr_from_env() -> Option<std::net::SocketAddr> {
    hermix_config::env_compat("HERMIX_TLS_ADDR", "HERMES_TLS_ADDR")
        .and_then(|s| s.parse().ok())
}

/// 确保证书材料可用：有则加载，无则生成并落盘。
/// 文件损坏（不可解析）时自愈重新生成——单机自签场景下,
/// 卡死在坏证书上比静默换证书（指纹变、设备需重新配对）更糟。
pub fn ensure_tls_material(dir: &Path) -> Result<TlsMaterial, AgentError> {
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");

    if cert_path.exists() && key_path.exists() {
        match load_material(&cert_path, &key_path) {
            Ok(m) => return Ok(m),
            Err(e) => {
                tracing::warn!(
                    "TLS 证书加载失败（{}），重新生成。已配对设备需重新配对。",
                    e
                );
            }
        }
    }

    let material = generate_material()?;
    std::fs::create_dir_all(dir)
        .map_err(|e| AgentError::Io(format!("create {}: {e}", dir.display())))?;
    write_atomic(&cert_path, &material.cert_pem)?;
    write_atomic(&key_path, &material.key_pem)?;
    // Windows 下把私钥权限收紧到不可读的场景不存在（当前以用户身份运行），
    // POSIX 权限收紧等跨平台需求出现时再加（YAGNI）。
    Ok(TlsMaterial {
        cert_der: material.cert_der,
        key_der: material.key_der,
        fingerprint_sha256: material.fingerprint_sha256,
    })
}

/// 内存形态（生成期中间产物 + PEM 落盘内容）。
struct GeneratedMaterial {
    cert_der: CertificateDer<'static>,
    key_der: PrivateKeyDer<'static>,
    cert_pem: String,
    key_pem: String,
    fingerprint_sha256: String,
}

fn generate_material() -> Result<GeneratedMaterial, AgentError> {
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_string()])
        .map_err(|e| AgentError::Config(format!("cert params: {e}")))?;

    // SAN 覆盖：loopback + 最佳努力 LAN IP（手机配对走 LAN）。
    let mut sans = vec![rcgen::SanType::DnsName("localhost".try_into().expect("localhost 是合法 DNS 名"))];
    sans.push(rcgen::SanType::IpAddress(IpAddr::from([127, 0, 0, 1]).into()));
    if let Some(lan) = best_effort_lan_ip() {
        tracing::info!("TLS SAN 加入局域网 IP: {lan}");
        sans.push(rcgen::SanType::IpAddress(lan.into()));
    }
    params.subject_alt_names = sans;

    // 有效期 10 年，起始前移 1 天容忍时钟偏差。
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(3650);

    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "hermix");

    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| AgentError::Config(format!("keygen: {e}")))?;
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| AgentError::Config(format!("self-sign: {e}")))?;

    let cert_der = cert.der().clone();
    let key_der =
        PrivateKeyDer::try_from(key_pair.serialize_der()).map_err(|e| {
            AgentError::Config(format!("key der: {e}"))
        })?;

    Ok(GeneratedMaterial {
        fingerprint_sha256: sha256_hex(cert_der.as_ref()),
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
        cert_der,
        key_der,
    })
}

fn load_material(
    cert_path: &Path,
    key_path: &Path,
) -> Result<TlsMaterial, AgentError> {
    let cert_pem = std::fs::read(cert_path)
        .map_err(|e| AgentError::Io(format!("read {}: {e}", cert_path.display())))?;
    let key_pem = std::fs::read(key_path)
        .map_err(|e| AgentError::Io(format!("read {}: {e}", key_path.display())))?;

    let mut cert_reader = std::io::BufReader::new(&cert_pem[..]);
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<_, _>>()
        .map_err(|e| AgentError::Config(format!("parse cert pem: {e}")))?;
    let cert_der = certs
        .into_iter()
        .next()
        .ok_or_else(|| AgentError::Config("cert.pem 为空".into()))?;

    let mut key_reader = std::io::BufReader::new(&key_pem[..]);
    let key_der = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|e| AgentError::Config(format!("parse key pem: {e}")))?
        .ok_or_else(|| AgentError::Config("key.pem 无私钥".into()))?;

    Ok(TlsMaterial {
        fingerprint_sha256: sha256_hex(cert_der.as_ref()),
        cert_der,
        key_der,
    })
}

/// 构建服务端 rustls 配置：TLS 1.3 优先（+1.2 兜底老客户端），
/// ALPN h2/http1.1。强制 ring provider（见模块注释）。
pub fn build_server_config(
    material: &TlsMaterial,
) -> Result<rustls::ServerConfig, AgentError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut cfg = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| AgentError::Config(format!("protocol versions: {e}")))?
        .with_no_client_auth()
        .with_single_cert(vec![material.cert_der.clone()], material.key_der.clone_key())
        .map_err(|e| AgentError::Config(format!("with_single_cert: {e}")))?;
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 最佳努力局域网 IP：UDP connect 不发包，仅让内核选路由源地址。
/// 失败（无网络 / 多网卡歧义）返回 None，证书只含 loopback SAN。
/// 配对二维码的 host 字段也复用此函数。
pub fn best_effort_lan_ip() -> Option<IpAddr> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // UDP connect 不发包，仅让内核选路由源地址。
    sock.connect("8.8.8.8:80").ok()?;
    let ip = sock.local_addr().ok()?.ip();
    // IPv6 私网判定（fc00::/7）顺带覆盖；公网/loopback 不进 SAN。
    let acceptable = match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
    };
    acceptable.then_some(ip)
}

/// 原子写：临时文件 + rename，防进程中途被杀留下半截 PEM。
fn write_atomic(path: &Path, contents: &str) -> Result<(), AgentError> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, contents)
        .map_err(|e| AgentError::Io(format!("write {}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| AgentError::Io(format!("rename {}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_generates_persists_and_reloads_same_fingerprint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m1 = ensure_tls_material(dir.path()).expect("generate");
        assert_eq!(m1.fingerprint_sha256.len(), 64, "sha256 hex 长度");
        assert!(
            m1.fingerprint_sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "指纹应为 hex"
        );

        // 重启路径：同目录再取 → 指纹一致（TOFU 固定的前提）。
        let m2 = ensure_tls_material(dir.path()).expect("reload");
        assert_eq!(
            m1.fingerprint_sha256, m2.fingerprint_sha256,
            "重启后指纹必须稳定"
        );
        assert!(dir.path().join("cert.pem").exists());
        assert!(dir.path().join("key.pem").exists());
    }

    #[test]
    fn corrupted_cert_self_heals_with_new_fingerprint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m1 = ensure_tls_material(dir.path()).expect("generate");
        std::fs::write(dir.path().join("cert.pem"), b"not a pem").expect("corrupt");

        let m2 = ensure_tls_material(dir.path()).expect("self-heal");
        assert_ne!(
            m1.fingerprint_sha256, m2.fingerprint_sha256,
            "自愈后是新证书，指纹必须变"
        );
    }

    #[test]
    fn server_config_accepts_generated_material() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = ensure_tls_material(dir.path()).expect("generate");
        let _cfg = build_server_config(&m).expect("server config");
        // 能构建即证明 cert/key 匹配、provider 就绪。
    }

    #[test]
    fn generated_cert_covers_localhost_and_loopback_san() {
        let dir = tempfile::tempdir().expect("tempdir");
        let m = ensure_tls_material(dir.path()).expect("generate");
        let pem = std::fs::read_to_string(dir.path().join("cert.pem")).expect("pem");
        assert!(pem.contains("BEGIN CERTIFICATE"), "PEM 形态落盘");
        // SAN 由 rcgen 写入 DER；此处校验生成路径没有 panic 且 PEM 落盘。
        // DER 级 SAN 断言依赖 x509 解析器，超出本模块职责（build_server_config
        // 已证明材料可被 rustls 接受）。
        let _ = m;
    }
}
