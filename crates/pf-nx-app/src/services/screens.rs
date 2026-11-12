//! The host's screens for this device: `GET /api/v1/display/targets` and the write of a pick,
//! on the library's mTLS lane. A host without the route has no Screen section.

use pf_client_core::display_targets::{Pick, Targets};

use crate::services::library::{agent, base_url, classify, get_json, LibraryError};
use crate::services::store::shared;

/// The host's answer as sent, `may_choose` and `role` included; `null` is no answer.
type Answer = Option<Targets>;

/// What this host lets this device stream. `None` on any miss, never an error.
pub(crate) fn fetch(addr: &str, mgmt_port: u16, identity: &(String, String), pin: Option<[u8; 32]>) -> Answer {
    let path = "/api/v1/display/targets";
    get_json::<Answer>(addr, mgmt_port, identity, pin, path, crate::services::budget::REQUEST)
        .inspect_err(|e| tracing::debug!("display targets ({addr}:{mgmt_port}): {e:?}"))
        .ok()
        .flatten()
}

/// `PUT /api/v1/display/clients/{this device's fingerprint}` with a row's pick. `Err` is the
/// phrase a notice frames: a refusal names its code, as the desktop's does.
pub(crate) fn put_pick(
    addr: &str,
    mgmt_port: u16,
    identity: &(String, String),
    pin: Option<[u8; 32]>,
    pick: &Pick,
) -> Result<(), String> {
    let (path, body) = pick_request(&identity.0, pick).map_err(|e| e.to_string())?;
    let agent = agent(identity, pin).map_err(|e| e.to_string())?;
    let url = format!("{}{path}", base_url(addr, mgmt_port));
    match agent
        .put(url.as_str())
        .header("content-type", "application/json")
        .send(body.as_str())
    {
        Ok(_) => Ok(()),
        Err(ureq::Error::StatusCode(code)) => Err(pf_l10n::client::error_mgmt_refused(code)),
        Err(e) => Err(classify(e).to_string()),
    }
}

/// The write's path and body: this device's own row, named by its certificate's fingerprint.
fn pick_request(cert_pem: &str, pick: &Pick) -> Result<(String, String), LibraryError> {
    let device = punktfunk_core::quic::endpoint::fingerprint_of_pem(cert_pem)
        .map_err(|e| LibraryError::Identity(format!("client cert fingerprint: {e}")))?;
    let path = format!("/api/v1/display/clients/{}", shared::hex(device));
    Ok((path, serde_json::json!(pick).to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pf_client_core::display_targets::{DisplayTarget, Role};

    /// The console gets `may_choose` and `role` as the host sent them; `null` is no section.
    #[test]
    fn the_answer_reaches_the_console_whole() {
        let body = r#"{ "may_choose": false, "host_default": "own", "targets": [{ "id": "own", "label": "Own" }],
            "role": { "yours": "second", "host_default": "main", "offered": ["only", "main", "second", "fourth"] } }"#;
        let t = serde_json::from_str::<Answer>(body).unwrap().unwrap();
        assert!(!t.may_choose);
        let role = t.role.unwrap();
        assert_eq!((role.yours, role.host_default), (Some(Role::Second), Role::Main));
        assert_eq!(role.offered, [Role::Only, Role::Main, Role::Second]);
        assert!(
            serde_json::from_str::<Answer>(r#"{"targets": []}"#)
                .unwrap()
                .unwrap()
                .may_choose
        );
        assert_eq!(serde_json::from_str::<Answer>("null").unwrap(), None);
    }

    /// A pick writes this device's own row: the screen, and a role's topology beside it.
    #[test]
    fn a_pick_writes_this_devices_row() {
        let (cert, _) = punktfunk_core::quic::endpoint::generate_identity().unwrap();
        let fp = shared::hex(punktfunk_core::quic::endpoint::fingerprint_of_pem(&cert).unwrap());
        let role = Pick {
            screen: DisplayTarget::Own,
            topology: Some("extend".into()),
        };
        let (path, body) = pick_request(&cert, &role).unwrap();
        assert_eq!(path, format!("/api/v1/display/clients/{fp}"));
        assert_eq!(body, r#"{"screen":"own","topology":"extend"}"#);
        let monitor = Pick {
            screen: DisplayTarget::Monitor("HDMI-A-1".into()),
            topology: None,
        };
        assert_eq!(
            pick_request(&cert, &monitor).unwrap().1,
            r#"{"screen":{"monitor":"HDMI-A-1"}}"#
        );
        assert!(matches!(pick_request("junk", &monitor), Err(LibraryError::Identity(_))));
    }
}
