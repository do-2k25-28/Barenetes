//! `barenetes-pki`: a small, dependency-light bootstrap tool for the
//! cluster's private mTLS PKI. Deliberately not a `barectl` subcommand or a
//! shell script; see docs/mtls-plan.md section 3 for why.
use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use rcgen::{
    BasicConstraints, CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyPair, KeyUsagePurpose,
};
use time::{Duration, OffsetDateTime};

#[derive(Parser)]
#[command(
    name = "barenetes-pki",
    version,
    about = "Bootstrap tool for the Barenetes cluster's private mTLS PKI"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new cluster certificate authority.
    InitCa(InitCaArgs),
    /// Issue a leaf certificate signed by an existing certificate authority.
    Issue(IssueArgs),
}

#[derive(Args)]
struct InitCaArgs {
    /// Directory to write ca.pem and ca-key.pem into.
    #[arg(long)]
    out_dir: PathBuf,

    /// Subject common name for the CA certificate.
    #[arg(long, default_value = "Barenetes cluster CA")]
    common_name: String,

    /// Validity period in days.
    #[arg(long, default_value_t = 3650)]
    days: i64,
}

/// The cluster role a leaf certificate is authorized for, recorded in its
/// subject's Organizational Unit. The API server's per-RPC authorization
/// (see `api/src/tls_identity.rs`) trusts this field, not the CN: CN is the
/// claimed *identity* (compared against a request's `node_name`), OU is the
/// claimed *role* (compared against which RPCs that role may call). Every
/// cluster certificate authenticates against the same CA, so without this
/// separate field any leaf cert -- including one issued for a worker node --
/// would be authorized for every RPC.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Role {
    /// The API server's own identity. Never itself a caller of any RPC.
    Api,
    /// The scheduler: WatchPods, WatchNodes, AssignPod.
    Scheduler,
    /// A human operator or automation driving `barectl`: CreatePod,
    /// DeletePod, GetPod, ListPods, GetNode, ListNodes.
    Cli,
    /// A worker node's agent. `--cn` must be that node's name, since the
    /// agent-facing RPCs also check CN against the claimed node_name.
    Node,
}

impl Role {
    fn as_str(self) -> &'static str {
        match self {
            Role::Api => "api",
            Role::Scheduler => "scheduler",
            Role::Cli => "cli",
            Role::Node => "node",
        }
    }
}

#[derive(Args)]
struct IssueArgs {
    /// Directory containing ca.pem and ca-key.pem.
    #[arg(long)]
    ca_dir: PathBuf,

    /// Common name for the leaf certificate: the cluster role (api,
    /// scheduler, barectl) or the real node_name for an agent. This is the
    /// identity the API server's mTLS handler compares node_name against.
    #[arg(long)]
    cn: String,

    /// The cluster role this certificate is authorized for. Determines
    /// which RPCs the API server accepts it for, independent of `--cn`.
    #[arg(long, value_enum)]
    role: Role,

    /// Directory to write <cn>.pem and <cn>-key.pem into.
    #[arg(long)]
    out_dir: PathBuf,

    /// Extra subject alt name, format DNS:<name> or IP:<addr> (repeatable).
    /// The common name is always included as a DNS SAN, so this is only
    /// needed for additional names/addresses a client might connect
    /// through.
    #[arg(long = "san", value_parser = parse_san)]
    sans: Vec<String>,

    /// Validity period in days.
    #[arg(long, default_value_t = 397)]
    days: i64,
}

/// Validates `DNS:<name>` / `IP:<addr>` and returns the bare name/address.
/// `CertificateParams::new` classifies plain strings as `IpAddress` or
/// `DnsName` on its own, so the prefix only needs to be checked here, not
/// re-encoded.
fn parse_san(raw: &str) -> Result<String, String> {
    let (kind, value) = raw
        .split_once(':')
        .ok_or_else(|| format!("invalid --san \"{raw}\", expected DNS:<name> or IP:<addr>"))?;
    match kind {
        "DNS" => Ok(value.to_string()),
        "IP" => {
            value
                .parse::<IpAddr>()
                .map_err(|_| format!("invalid --san IP address \"{value}\""))?;
            Ok(value.to_string())
        }
        other => Err(format!(
            "invalid --san kind \"{other}\", expected DNS or IP"
        )),
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::InitCa(args) => init_ca(args),
        Command::Issue(args) => issue(args),
    }
}

/// Writes `contents` to `path` with `mode` set from the file's creation, not
/// after the fact: `fs::write` + `set_permissions` briefly leaves the file at
/// the default 0666-minus-umask (e.g. 0644 for a normal 022 umask), during
/// which another local process could read a private key before its
/// restrictive mode lands. Writing to a sibling temp file created with the
/// target mode via `OpenOptionsExt::mode`, then renaming it into place,
/// closes that window and also means a reader never observes a partially
/// written file.
fn write_pem(path: &Path, contents: &str, mode: u32) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp_path = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("pem"),
        std::process::id()
    ));

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp_path)
        .with_context(|| format!("creating {}", tmp_path.display()))?;
    file.write_all(contents.as_bytes())
        .with_context(|| format!("writing {}", tmp_path.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", tmp_path.display()))?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("renaming {} to {}", tmp_path.display(), path.display()))?;
    Ok(())
}

fn validity(days: i64) -> (OffsetDateTime, OffsetDateTime) {
    let now = OffsetDateTime::now_utc();
    (now, now + Duration::days(days))
}

fn init_ca(args: InitCaArgs) -> Result<()> {
    let ca_path = args.out_dir.join("ca.pem");
    let ca_key_path = args.out_dir.join("ca-key.pem");

    if ca_path.exists() {
        bail!(
            "{} already exists; refusing to overwrite (this tool has no --force / rotation support yet)",
            ca_path.display()
        );
    }

    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating {}", args.out_dir.display()))?;

    let key = KeyPair::generate().context("generating CA key pair")?;

    let mut params = CertificateParams::new(Vec::<String>::new()).context("building CA params")?;
    let (not_before, not_after) = validity(args.days);
    params.not_before = not_before;
    params.not_after = not_after;
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, args.common_name);
    params.distinguished_name = dn;

    let cert = params.self_signed(&key).context("self-signing CA cert")?;

    write_pem(&ca_path, &cert.pem(), 0o644)?;
    write_pem(&ca_key_path, &key.serialize_pem(), 0o600)?;

    println!("wrote {}", ca_path.display());
    println!("wrote {}", ca_key_path.display());
    Ok(())
}

fn issue(args: IssueArgs) -> Result<()> {
    let ca_pem = fs::read_to_string(args.ca_dir.join("ca.pem"))
        .with_context(|| format!("reading {}", args.ca_dir.join("ca.pem").display()))?;
    let ca_key_pem = fs::read_to_string(args.ca_dir.join("ca-key.pem"))
        .with_context(|| format!("reading {}", args.ca_dir.join("ca-key.pem").display()))?;
    let ca_key = KeyPair::from_pem(&ca_key_pem).context("parsing CA private key")?;
    let issuer = Issuer::from_ca_cert_pem(&ca_pem, ca_key).context("parsing CA certificate")?;

    let mut subject_alt_names = vec![args.cn.clone()];
    subject_alt_names.extend(args.sans);

    let mut params =
        CertificateParams::new(subject_alt_names).context("building leaf cert params")?;
    let (not_before, not_after) = validity(args.days);
    params.not_before = not_before;
    params.not_after = not_after;
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    // Every leaf cert this tool issues may end up on either side of an mTLS
    // handshake (api/agent as servers, scheduler/barectl/agent as future
    // clients), so both EKUs are set rather than trying to track which
    // role needs which.
    params.extended_key_usages = vec![
        ExtendedKeyUsagePurpose::ServerAuth,
        ExtendedKeyUsagePurpose::ClientAuth,
    ];
    let mut dn = DistinguishedName::new();
    dn.push(DnType::CommonName, args.cn.clone());
    dn.push(DnType::OrganizationalUnitName, args.role.as_str());
    params.distinguished_name = dn;

    let leaf_key = KeyPair::generate().context("generating leaf key pair")?;
    let cert = params
        .signed_by(&leaf_key, &issuer)
        .context("signing leaf cert")?;

    fs::create_dir_all(&args.out_dir)
        .with_context(|| format!("creating {}", args.out_dir.display()))?;
    let cert_path = args.out_dir.join(format!("{}.pem", args.cn));
    let key_path = args.out_dir.join(format!("{}-key.pem", args.cn));

    write_pem(&cert_path, &cert.pem(), 0o644)?;
    write_pem(&key_path, &leaf_key.serialize_pem(), 0o600)?;

    println!("wrote {}", cert_path.display());
    println!("wrote {}", key_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("barenetes-pki-test-{label}-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn issue_produces_a_cert_signed_by_the_ca_with_the_requested_cn() {
        let ca_dir = tempdir("ca");
        init_ca(InitCaArgs {
            out_dir: ca_dir.clone(),
            common_name: "Test CA".to_string(),
            days: 3650,
        })
        .unwrap();

        let out_dir = tempdir("leaf");
        issue(IssueArgs {
            ca_dir: ca_dir.clone(),
            cn: "node-a".to_string(),
            role: Role::Node,
            out_dir: out_dir.clone(),
            sans: vec![],
            days: 397,
        })
        .unwrap();

        let leaf_pem = fs::read_to_string(out_dir.join("node-a.pem")).unwrap();
        let leaf_der = pem_to_der(&leaf_pem);
        let (_, leaf_x509) = x509_parser::parse_x509_certificate(&leaf_der).unwrap();
        let cn = leaf_x509
            .subject()
            .iter_common_name()
            .next()
            .and_then(|cn| cn.as_str().ok())
            .unwrap();
        assert_eq!(cn, "node-a");
        let ou = leaf_x509
            .subject()
            .iter_organizational_unit()
            .next()
            .and_then(|ou| ou.as_str().ok())
            .unwrap();
        assert_eq!(ou, "node");

        let ca_pem = fs::read_to_string(ca_dir.join("ca.pem")).unwrap();
        let ca_der = pem_to_der(&ca_pem);
        let (_, ca_x509) = x509_parser::parse_x509_certificate(&ca_der).unwrap();

        assert_eq!(leaf_x509.issuer(), ca_x509.subject());

        fs::remove_dir_all(&ca_dir).ok();
        fs::remove_dir_all(&out_dir).ok();
    }

    fn pem_to_der(pem_str: &str) -> Vec<u8> {
        pem::parse(pem_str).unwrap().into_contents()
    }
}
