//! The lease files (ADR 0015): one JSON file per lease, keyed by workload id, in
//! `<home>/leases/` or, with an agent key, in `leases/` beside the key file.
//!
//! A file holds the lease's root secret, which is everything that controls the lease, so it
//! is plaintext, mode 0600, never printed and never deleted by `toon`. The file is written
//! before the spawn packet leaves; a spawn whose file cannot be written is not sent.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use zeroize::Zeroizing;

use crate::node;
use crate::outcome::{Error, ErrorCode};

/// Where a lease stands, as `toon` last learned it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// The provider took the spawn.
    Live,
    /// The provider answered the spawn with an error: it was billed all the same.
    Failed,
    /// Nobody answered the spawn, so the lease may or may not exist. `status` settles it.
    Unknown,
    /// The lease is over: terminated, or the provider says it expired.
    Ended,
}

impl State {
    pub fn name(self) -> &'static str {
        match self {
            State::Live => "live",
            State::Failed => "failed",
            State::Unknown => "unknown",
            State::Ended => "ended",
        }
    }

    fn parse(name: &str) -> Option<Self> {
        [State::Live, State::Failed, State::Unknown, State::Ended]
            .into_iter()
            .find(|state| state.name() == name)
    }
}

/// The Deployment (kind 30437) a lease publishes and refreshes: what `toon deployment publish`
/// needs besides what the lease itself holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    pub environment: String,
    pub repo: String,
    pub commit: String,
    pub url: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Lease {
    pub workload_id: String,
    /// 64 hex characters: the secret every Continuation Token of the lease derives from.
    pub root_secret: Zeroizing<String>,
    /// The provider's public key, 64 hex characters.
    pub provider: String,
    pub ilp_address: String,
    /// Where the provider's connector describes itself; where the peering is found.
    pub connector_url: String,
    /// The connector's sealing key, `0x04…`, which every packet of the lease is sealed to.
    pub connector_seal_key: String,
    /// The Listing's coordinate, `30432:<pubkey>:<d>`.
    pub listing: String,
    pub listing_name: String,
    pub listing_version: u64,
    /// What one Lease Interval costs at this version, in base units: what an extend pays.
    /// `None` for a file that states no price and paid nothing to show one.
    pub price: Option<u64>,
    pub role: String,
    pub expires_at: Option<u64>,
    /// What has been paid for the lease, in base units.
    pub paid: u64,
    /// The most the lease may pay in total, if one was set.
    pub lease_limit: Option<u64>,
    pub state: State,
    /// The error that left the lease `failed`, as the provider or the connector gave it.
    pub error: Option<(String, String)>,
    pub created_at: u64,
    /// The Deployment to publish with this lease, if `--deployment` named an environment.
    pub deployment: Option<Claim>,
}

impl Lease {
    /// What `list` and the JSON of a command show: everything but the root secret.
    pub fn public(&self) -> Value {
        json!({
            "workload_id": self.workload_id,
            "provider": self.provider,
            "ilp_address": self.ilp_address,
            "listing": self.listing,
            "listing_version": self.listing_version,
            "role": self.role,
            "expires_at": self.expires_at,
            "paid": self.paid,
            "lease_limit": self.lease_limit,
            "state": self.state.name(),
            "error": self.error.as_ref().map(|(code, message)| json!({ "code": code, "message": message })),
            "created_at": self.created_at,
            "deployment": self.deployment.as_ref().map(|claim| &claim.environment),
        })
    }

    fn to_json(&self) -> Value {
        let mut file = self.public();
        file["version"] = json!(1);
        file["root_secret"] = json!(&*self.root_secret);
        file["connector_url"] = json!(self.connector_url);
        file["connector_seal_key"] = json!(self.connector_seal_key);
        file["listing_name"] = json!(self.listing_name);
        file["price"] = json!(self.price);
        file["deployment"] = match &self.deployment {
            Some(claim) => json!({
                "environment": claim.environment,
                "repo": claim.repo,
                "commit": claim.commit,
                "url": claim.url,
            }),
            None => Value::Null,
        };
        file
    }

    fn from_json(file: &Value) -> Option<Self> {
        let text = |field: &str| file[field].as_str().map(str::to_owned);
        Some(Self {
            workload_id: text("workload_id")?,
            root_secret: Zeroizing::new(text("root_secret")?),
            provider: text("provider")?,
            ilp_address: text("ilp_address")?,
            connector_url: text("connector_url")?,
            connector_seal_key: text("connector_seal_key")?,
            listing: text("listing")?,
            listing_name: text("listing_name")?,
            listing_version: file["listing_version"].as_u64()?,
            // A file written before extends existed paid one interval, if it paid at all: that
            // was the price.
            price: file["price"]
                .as_u64()
                .or_else(|| file["paid"].as_u64().filter(|paid| *paid > 0)),
            role: text("role")?,
            expires_at: file["expires_at"].as_u64(),
            paid: file["paid"].as_u64()?,
            lease_limit: file["lease_limit"].as_u64(),
            state: State::parse(file["state"].as_str()?)?,
            error: match &file["error"] {
                Value::Null => None,
                error => Some((
                    error["code"].as_str()?.to_owned(),
                    error["message"].as_str()?.to_owned(),
                )),
            },
            created_at: file["created_at"].as_u64()?,
            deployment: match &file["deployment"] {
                Value::Null => None,
                claim => Some(Claim {
                    environment: claim["environment"].as_str()?.to_owned(),
                    repo: claim["repo"].as_str()?.to_owned(),
                    commit: claim["commit"].as_str()?.to_owned(),
                    url: claim["url"].as_str().map(str::to_owned),
                }),
            },
        })
    }

    /// The root secret as bytes.
    pub fn root(&self) -> Result<[u8; 32], Error> {
        hex::decode(self.root_secret.as_str())
            .ok()
            .and_then(|bytes| bytes.try_into().ok())
            .ok_or_else(|| {
                io_error(format!(
                    "The lease file of {} holds no root secret of 64 hex characters.",
                    self.workload_id
                ))
            })
    }

    /// The connector's sealing key, which every packet of the lease is sealed to.
    pub fn seal_key(&self) -> Result<[u8; 65], Error> {
        crate::listing::seal_key(&self.connector_seal_key).ok_or_else(|| {
            io_error(format!(
                "The lease file of {} holds no connector seal key of 65 bytes.",
                self.workload_id
            ))
        })
    }
}

fn io_error(message: String) -> Error {
    Error {
        nothing_sent: false,
        unanswered: None,
        code: ErrorCode::Io,
        message,
    }
}

/// The directory a node's lease files are kept in.
#[derive(Clone, Debug)]
pub struct Leases {
    dir: PathBuf,
}

impl Leases {
    /// `<home>/leases/`.
    pub fn of_home(home: &Path) -> Self {
        Self {
            dir: home.join("leases"),
        }
    }

    /// `leases/` beside the agent key file.
    pub fn beside(key_file: &Path) -> Self {
        let parent = match key_file.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => parent,
            _ => Path::new("."),
        };
        Self {
            dir: parent.join("leases"),
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, workload_id: &str) -> Result<PathBuf, Error> {
        let valid = workload_id.len() == 64
            && workload_id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if !valid {
            return Err(Error {
                nothing_sent: true,
                unanswered: None,
                code: ErrorCode::Usage,
                message: format!(
                    "{workload_id} is not a workload id: it is 64 lowercase hex characters."
                ),
            });
        }
        Ok(self.dir.join(format!("{workload_id}.json")))
    }

    /// Write the lease in full or not at all, mode 0600. A failure sent nothing.
    pub fn write(&self, lease: &Lease) -> Result<(), Error> {
        let path = self.path(&lease.workload_id)?;
        let text = Zeroizing::new(lease.to_json().to_string());
        node::write(&path, text.as_bytes(), 0o600).map_err(|error| Error {
            nothing_sent: true,
            message: format!(
                "The lease file could not be written, so nothing was sent: {}",
                error.message
            ),
            ..error
        })
    }

    /// The lease `workload_id` names, or `lease_not_held`.
    pub fn read(&self, workload_id: &str) -> Result<Lease, Error> {
        let path = self.path(workload_id)?;
        let text = match fs::read_to_string(&path) {
            Ok(text) => Zeroizing::new(text),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error {
                    nothing_sent: true,
                    unanswered: None,
                    code: ErrorCode::LeaseNotHeld,
                    message: format!(
                        "This node holds no lease {workload_id}: there is no file for it in {}. \
                         A lease whose file was lost cannot be read, extended or ended from here.",
                        self.dir.display()
                    ),
                })
            }
            Err(source) => return Err(io_error(format!("{}: {source}.", path.display()))),
        };
        parse(&text).ok_or_else(|| {
            io_error(format!(
                "{} is not a lease file this version reads.",
                path.display()
            ))
        })
    }

    /// Every lease file, by workload id. A file that is not a lease file is an error, not
    /// skipped: it may be a lease the operator is relying on.
    pub fn list(&self) -> Result<Vec<Lease>, Error> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(source) => return Err(io_error(format!("{}: {source}.", self.dir.display()))),
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter_map(|name| name.strip_suffix(".json").map(str::to_owned))
            .collect();
        names.sort();
        names.iter().map(|name| self.read(name)).collect()
    }

    pub fn remove(&self, workload_id: &str) -> Result<(), Error> {
        let path = self.path(workload_id)?;
        fs::remove_file(&path).map_err(|source| io_error(format!("{}: {source}.", path.display())))
    }
}

fn parse(text: &str) -> Option<Lease> {
    Lease::from_json(&serde_json::from_str(text).ok()?)
}

/// Every lease file of `leases` as `(file name, contents)`, for a backup.
pub fn files(leases: &Leases) -> Result<Vec<(String, Zeroizing<String>)>, Error> {
    let mut found = Vec::new();
    for lease in leases.list()? {
        let path = leases.path(&lease.workload_id)?;
        let text = fs::read_to_string(&path)
            .map(Zeroizing::new)
            .map_err(|source| io_error(format!("{}: {source}.", path.display())))?;
        found.push((format!("{}.json", lease.workload_id), text));
    }
    Ok(found)
}

/// Write the lease files of a backup into `leases`, each mode 0600; a file already there is
/// an error.
pub fn restore(leases: &Leases, files: &[(String, String)]) -> Result<Vec<PathBuf>, Error> {
    let mut written = Vec::new();
    for (name, text) in files {
        let lease = parse(text).ok_or_else(|| {
            io_error("The backup holds a lease file this version does not read.".into())
        })?;
        if format!("{}.json", lease.workload_id) != *name {
            return Err(io_error(format!(
                "The backup's lease file {name} is for another workload."
            )));
        }
        let path = leases.path(&lease.workload_id)?;
        if path.exists() {
            return Err(io_error(format!("{} is there already.", path.display())));
        }
        node::write(&path, text.as_bytes(), 0o600)?;
        written.push(path);
    }
    Ok(written)
}
