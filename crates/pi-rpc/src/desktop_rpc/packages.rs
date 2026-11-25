//! Pi's packages on the Shell's side: `list_packages` reads the configured
//! ones, `install_package` and `remove_package` run the same install and
//! removal as `gentle-shell install|remove <source>` there (the same
//! validation, the same reload afterwards) as a job whose output arrives as
//! `package_progress` events and whose end as `package_finished`.

use super::{DesktopRpcError, PackageScope, decode_data};
use serde::Deserialize;
use serde_json::{Value, json};

/// The event a package job's output line arrives as.
pub const PACKAGE_PROGRESS: &str = "package_progress";
/// The event a package job's end arrives as.
pub const PACKAGE_FINISHED: &str = "package_finished";

/// One configured package, as Pi's settings list it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct InstalledPackage {
    pub source: String,
    pub version: Option<String>,
    pub scope: PackageScope,
}

/// The decoded `list_packages` data.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PackageList {
    pub packages: Vec<InstalledPackage>,
}

impl PackageList {
    pub(super) fn from_data(data: &Value) -> Result<Self, DesktopRpcError> {
        decode_data(data)
    }
}

/// The decoded `install_package` / `remove_package` data: the job its
/// events name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageJob {
    pub job_id: String,
}

/// The decoded `package_progress` event: one output line.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageProgress {
    pub job_id: String,
    pub line: String,
}

/// The decoded `package_finished` event. `error` is the Shell's words.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageFinished {
    pub job_id: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Decode a record naming a job, refusing an empty job id.
pub(super) fn job_record<T: serde::de::DeserializeOwned>(
    data: &Value,
    job_id: impl Fn(&T) -> &str,
) -> Result<T, DesktopRpcError> {
    let record: T = decode_data(data)?;
    if job_id(&record).is_empty() {
        return Err(DesktopRpcError::MalformedPayload(
            "a package record has an empty jobId".into(),
        ));
    }
    Ok(record)
}

/// The wire `params` of `install_package` and `remove_package`:
/// `{ source }`, plus `scope` when given (the Shell's default otherwise).
pub(super) fn package_params(
    source: &str,
    scope: Option<PackageScope>,
) -> Result<Value, DesktopRpcError> {
    if source.trim().is_empty() {
        return Err(DesktopRpcError::InvalidValue {
            field: "source",
            value: source.to_string(),
        });
    }
    let mut params = json!({ "source": source });
    if let Some(scope) = scope {
        params["scope"] = scope.as_str().into();
    }
    Ok(params)
}
