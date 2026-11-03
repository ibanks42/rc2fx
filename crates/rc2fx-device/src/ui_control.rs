//! Private, owner-only AF_UNIX adapter for the hardware touchscreen.
//!
//! The small versioned wire image avoids putting a JSON parser or Rust/C++ ABI
//! into RODE's GUI. Every mutation still goes through the SAME Api/coordinator
//! as USB; there is no second Store, native client, or audio runtime.
//!
//! Each packet has a u32 big-endian byte length. Request: RCUI, version=1,
//! opcode (0 snapshot, 1 apply preset, 2 processing), enabled (0/1), id length,
//! expected revision u64, expected generation u64, then ASCII preset ID.
//! Reply: RCUI, version=1, flags, status u16, revision u64, generation u64,
//! preset count u16, message length u16, message UTF-8, then each preset as
//! id length u8, name length u8, id/name UTF-8. All integers are big-endian.
//! Flags: 1 processing known, 2 enabled, 4 presets ready, 8 applied,
//! 16 explicit processing-OFF allowed, 32 processing-ON allowed.
use crate::api::Reply;
use crate::{
    api::{Api, Options},
    coordinator::Handle,
};
use rc2fx_core::{DeviceState, EnginePhase, RoutingPhase, SelectionPhase};
use rc2fx_usb::Request;
use serde_json::json;
use std::{
    io,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::Semaphore,
    task::JoinSet,
};

const MAGIC: &[u8; 4] = b"RCUI";
const MAX_REQUEST: usize = 24 + 48;
const MAX_REPLY: usize = 32 * 1024;
const MAX_CLIENTS: usize = 4;
const IO_WAIT: Duration = Duration::from_secs(10);
const OP_WAIT: Duration = Duration::from_secs(15);

struct SocketFile {
    path: PathBuf,
    dev: u64,
    ino: u64,
}
impl Drop for SocketFile {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.path)
            .is_ok_and(|m| m.dev() == self.dev && m.ino() == self.ino)
        {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
fn error(status: u16, message: &str) -> Reply {
    // The API's error page, as validated raw JSON.
    crate::api::failure(status, message)
}

fn decode(bytes: &[u8]) -> Result<Option<Request>, Reply> {
    if bytes.len() < 24
        || &bytes[..4] != MAGIC
        || bytes[4] != 1
        || bytes[6] > 1
        || bytes.len() != 24 + usize::from(bytes[7])
        || bytes.len() > MAX_REQUEST
    {
        return Err(error(400, "Unsupported touchscreen request"));
    }
    let revision = u64::from_be_bytes(bytes[8..16].try_into().unwrap());
    let generation = u64::from_be_bytes(bytes[16..24].try_into().unwrap());
    let input = |method: &str, path: String, body| Request {
        method: method.into(),
        path,
        body,
    };
    match bytes[5] {
        0 if bytes.len() == 24 && bytes[6] == 0 && revision == 0 && generation == 0 => Ok(None),
        1 if bytes[6] == 0 && generation == 0 => {
            let id =
                std::str::from_utf8(&bytes[24..]).map_err(|_| error(400, "Invalid preset ID"))?;
            if !rc2fx_core::valid_id(id) {
                return Err(error(400, "Invalid preset ID"));
            }
            Ok(Some(input(
                "POST",
                format!("api/v1/presets/{id}/apply"),
                Some(json!({"expectedRevision": revision})),
            )))
        }
        2 if bytes.len() == 24 => Ok(Some(input(
            "PUT",
            "api/v1/routing".into(),
            Some(json!({
                "enabled": bytes[6] == 1, "expectedRevision": revision, "expectedGeneration": generation
            })),
        ))),
        _ => Err(error(400, "Unknown touchscreen command")),
    }
}

async fn dispatch(api: &mut Api, input: &[u8]) -> Vec<u8> {
    let reply = match decode(input) {
        Err(error) => error,
        Ok(None) => {
            api.request(Request {
                method: "GET".into(),
                path: "api/v1/state".into(),
                body: None,
            })
            .await
        }
        Ok(Some(request)) => api.request(request).await,
    };
    if !(200..300).contains(&reply.status) {
        return encode_error(reply);
    }
    let state: DeviceState = match serde_json::from_str(reply.body.get()) {
        Ok(state) => state,
        Err(_) => return encode_error(error(500, "Invalid controller state")),
    };
    let presets = api
        .request(Request {
            method: "GET".into(),
            path: "api/v1/presets".into(),
            body: None,
        })
        .await;
    if presets.status != 200 {
        return encode_error(presets);
    }
    let list: Vec<rc2fx_core::PresetSummary> =
        match serde_json::from_str::<serde_json::Value>(presets.body.get())
            .ok()
            .and_then(|value| serde_json::from_value(value.get("presets")?.clone()).ok())
        {
            Some(list) => list,
            None => return encode_error(error(500, "Invalid preset list")),
        };
    encode_state(reply.status, &state, &list)
}

fn header(
    status: u16,
    flags: u8,
    revision: u64,
    generation: u64,
    presets: u16,
    message: &str,
) -> Vec<u8> {
    // Do not split UTF-8, and never allocate an unbounded diagnostic response.
    let mut end = message.len().min(512);
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    let message = &message[..end];
    let mut bytes = Vec::with_capacity(28 + message.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&[1, flags]);
    bytes.extend_from_slice(&status.to_be_bytes());
    bytes.extend_from_slice(&revision.to_be_bytes());
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.extend_from_slice(&presets.to_be_bytes());
    bytes.extend_from_slice(&(message.len() as u16).to_be_bytes());
    bytes.extend_from_slice(message.as_bytes());
    bytes
}
fn encode_error(reply: Reply) -> Vec<u8> {
    header(
        reply.status,
        0,
        0,
        0,
        0,
        serde_json::from_str::<serde_json::Value>(reply.body.get())
            .ok()
            .and_then(|value| value.get("error")?.as_str().map(str::to_owned))
            .as_deref()
            .unwrap_or("Controller unavailable"),
    )
}
fn encode_state(
    status: u16,
    state: &DeviceState,
    presets: &[rc2fx_core::PresetSummary],
) -> Vec<u8> {
    if presets.len() > crate::store::MAX_PRESETS
        || presets
            .iter()
            .any(|p| !rc2fx_core::valid_id(&p.id) || p.name.len() > 128)
    {
        return encode_error(error(500, "Preset list exceeds touchscreen bounds"));
    }
    let known = matches!(
        state.routing.status,
        RoutingPhase::Active | RoutingPhase::Bypassed
    );
    let enabled = state.routing.status == RoutingPhase::Active;
    let ready = known
        && state.engine.status == EnginePhase::Running
        && state.engine.audio_advancing
        && state.engine.applied_revision == state.config.revision
        && state.selection.status == SelectionPhase::Idle
        && !state.config.selected_targets.is_empty();
    // Unlike DeviceState::is_applied(), the publication can be fully confirmed
    // while processing is intentionally bypassed. Do not label bypass an error.
    let applied = state.engine.audio_advancing
        && state.engine.applied_revision == state.config.revision
        && state.selection.status == SelectionPhase::Idle;
    let flags = u8::from(known)
        | u8::from(enabled) << 1
        | u8::from(ready) << 2
        | u8::from(applied) << 3
        | u8::from(state.can_disable_processing()) << 4
        | u8::from(state.can_enable_processing()) << 5;
    let mut bytes = header(
        status,
        flags,
        state.config.revision,
        state.engine.generation,
        presets.len() as u16,
        &state.engine.message,
    );
    for preset in presets {
        bytes.push(preset.id.len() as u8);
        bytes.push(preset.name.len() as u8);
        bytes.extend_from_slice(preset.id.as_bytes());
        bytes.extend_from_slice(preset.name.as_bytes());
    }
    bytes
}

async fn connection(
    mut stream: UnixStream,
    controller: Handle,
    options: Options,
) -> io::Result<()> {
    // Socket mode 0600 is the primary access control; also reject other UIDs.
    if stream.peer_cred()?.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::from(io::ErrorKind::PermissionDenied));
    }
    let mut api = Api::new(controller, options);
    loop {
        let length = tokio::time::timeout(IO_WAIT, stream.read_u32()).await?? as usize;
        if !(24..=MAX_REQUEST).contains(&length) {
            return Err(invalid("UI request length out of bounds"));
        }
        let mut request = vec![0; length];
        tokio::time::timeout(IO_WAIT, stream.read_exact(&mut request)).await??;
        let reply = match tokio::time::timeout(OP_WAIT, dispatch(&mut api, &request)).await {
            Ok(reply) => reply,
            Err(_) => encode_error(error(
                504,
                "Timed out; outcome may be unknown. Refresh before another change.",
            )),
        };
        if reply.len() > MAX_REPLY {
            return Err(invalid("UI reply exceeds bound"));
        }
        tokio::time::timeout(IO_WAIT, async {
            stream.write_u32(reply.len() as u32).await?;
            stream.write_all(&reply).await
        })
        .await??;
    }
}

/// One optional device-local listener, sharing the USB controller owner.
/// Does not unlink an existing socket, open a Store, or touch audio devices.
pub async fn serve(path: &Path, controller: Handle, options: Options) -> io::Result<()> {
    let listener = UnixListener::bind(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    let _cleanup = SocketFile {
        path: path.to_owned(),
        dev: metadata.dev(),
        ino: metadata.ino(),
    };
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let permits = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut tasks = JoinSet::new();
    loop {
        tokio::select! {
            incoming = listener.accept() => {
                let (stream, _) = incoming?;
                let Ok(permit) = permits.clone().try_acquire_owned() else { drop(stream); continue; };
                let handle = controller.clone();
                let options = options.clone();
                tasks.spawn(async move { let _permit = permit; let _ = connection(stream, handle, options).await; });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        controller::Controller,
        coordinator::{Coordinator, receive},
        store::Store,
    };
    use rc2fx_core::Config;
    fn packet(op: u8, enabled: u8, revision: u64, generation: u64, id: &[u8]) -> Vec<u8> {
        let mut b = b"RCUI".to_vec();
        b.extend_from_slice(&[1, op, enabled, id.len() as u8]);
        b.extend_from_slice(&revision.to_be_bytes());
        b.extend_from_slice(&generation.to_be_bytes());
        b.extend_from_slice(id);
        b
    }
    #[test]
    fn strict_versioned_requests() {
        assert!(matches!(decode(&packet(0, 0, 0, 0, b"")), Ok(None)));
        let apply = decode(&packet(1, 0, 42, 0, b"voice")).unwrap().unwrap();
        assert_eq!(apply.path, "api/v1/presets/voice/apply");
        assert_eq!(apply.body.unwrap()["expectedRevision"], 42);
        let toggle = decode(&packet(2, 1, 42, 7, b"")).unwrap().unwrap();
        assert_eq!(
            toggle.body.unwrap(),
            json!({"enabled":true,"expectedRevision":42,"expectedGeneration":7})
        );
        for bad in [
            packet(3, 0, 0, 0, b""),
            packet(0, 0, 1, 0, b""),
            packet(1, 0, 1, 0, b"../x"),
            packet(2, 2, 1, 1, b""),
            packet(2, 1, 1, 1, b"x"),
        ] {
            assert_eq!(decode(&bad).err().unwrap().status, 400);
        }
        for len in 0..24 {
            assert!(decode(&vec![0; len]).is_err());
        }
    }
    async fn rpc(stream: &mut UnixStream, request: &[u8]) -> Vec<u8> {
        stream.write_u32(request.len() as u32).await.unwrap();
        stream.write_all(request).await.unwrap();
        let len = stream.read_u32().await.unwrap() as usize;
        assert!(len <= MAX_REPLY);
        let mut reply = vec![0; len];
        stream.read_exact(&mut reply).await.unwrap();
        reply
    }
    #[tokio::test]
    async fn actual_socket_shares_owner_and_revision_conflicts() {
        let directory = tempfile::tempdir().unwrap();
        let data = directory.path().join("data");
        let socket = directory.path().join("ui.sock");
        let (handle, owner) = Coordinator::new(
            Controller::new(Store::open(&data, Config::default()).unwrap()),
            None,
        )
        .unwrap();
        let thread = std::thread::spawn(move || owner.run());
        let initial = receive(handle.state()).await.unwrap();
        receive(handle.create_preset("Voice".into(), initial.config.revision))
            .await
            .unwrap();
        let task = tokio::spawn({
            let path = socket.clone();
            let h = handle.clone();
            async move { serve(&path, h, Options::default()).await }
        });
        let mut stream = loop {
            match UnixStream::connect(&socket).await {
                Ok(s) => break s,
                Err(_) => tokio::task::yield_now().await,
            }
        };
        assert_eq!(
            std::fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let reply = rpc(&mut stream, &packet(0, 0, 0, 0, b"")).await;
        assert_eq!(&reply[..4], MAGIC);
        assert_eq!(u16::from_be_bytes(reply[6..8].try_into().unwrap()), 200);
        assert_eq!(u16::from_be_bytes(reply[24..26].try_into().unwrap()), 1);
        let presets = receive(handle.presets()).await.unwrap();
        let revision = receive(handle.state()).await.unwrap().config.revision;
        let mut other = Api::new(handle.clone(), Options::default());
        let updated = other
            .request(Request {
                method: "POST".into(),
                path: format!("api/v1/presets/{}/apply", presets[0].id),
                body: Some(json!({"expectedRevision":revision})),
            })
            .await;
        assert_eq!(updated.status, 200);
        let stale = rpc(
            &mut stream,
            &packet(1, 0, revision, 0, presets[0].id.as_bytes()),
        )
        .await;
        assert_eq!(u16::from_be_bytes(stale[6..8].try_into().unwrap()), 409);
        assert_eq!(
            receive(handle.state()).await.unwrap().config.revision,
            revision + 1
        );
        // A failed second bind cannot unlink the real listener.
        assert!(
            serve(&socket, handle.clone(), Options::default())
                .await
                .is_err()
        );
        assert!(UnixStream::connect(&socket).await.is_ok());
        drop(stream);
        task.abort();
        let _ = task.await;
        handle.close();
        handle.shutdown().await.unwrap();
        thread.join().unwrap();
        assert!(!socket.exists());
    }

    #[test]
    fn native_fault_keeps_explicit_off_eligible() {
        use rc2fx_core::{EngineMode, RoutingPhase};
        let directory = tempfile::tempdir().unwrap();
        let controller = Controller::new(Store::open(directory.path(), Config::default()).unwrap());
        let mut state = controller.state();
        state.engine.mode = EngineMode::Native;
        state.engine.status = EnginePhase::Error;
        state.routing.status = RoutingPhase::Error;
        state.selection.status = SelectionPhase::Error;
        state.selection.pending_target_ids.clear();
        let view = encode_state(200, &state, &[]);
        assert_ne!(view[5] & 16, 0, "fault-time OFF remains available");
        assert_eq!(view[5] & (4 | 32), 0, "presets and ON remain unavailable");
    }

    /// Cross-language regression: the ACTUAL C serializer/decoder talks to
    /// this real controller, not a parallel model of the wire protocol.
    #[tokio::test]
    async fn actual_c_client_presets_conflicts_and_delayed_mutation_reply() {
        use std::io::{Read, Write};
        let directory = tempfile::tempdir().unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap();
        let executable = directory.path().join("ui-client-test");
        let built =
            std::process::Command::new(std::env::var_os("CC").unwrap_or_else(|| "cc".into()))
                .args(["-std=c11", "-O1", "-Wall", "-Wextra", "-Werror", "-pthread"])
                .arg(root.join("experiments/device-panel/client_test.c"))
                .arg("-o")
                .arg(&executable)
                .output()
                .unwrap();
        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );
        let data = directory.path().join("data");
        let socket = directory.path().join("ui.sock");
        let proxy_path = directory.path().join("delayed.sock");
        let (handle, owner) = Coordinator::new(
            Controller::new(Store::open(&data, Config::default()).unwrap()),
            None,
        )
        .unwrap();
        let owner_thread = std::thread::spawn(move || owner.run());
        let initial = receive(handle.state()).await.unwrap();
        receive(handle.create_preset("Voice".into(), initial.config.revision))
            .await
            .unwrap();
        let task = tokio::spawn({
            let p = socket.clone();
            let h = handle.clone();
            async move { serve(&p, h, Options::default()).await }
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if UnixStream::connect(&socket).await.is_ok() {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let proxy = std::os::unix::net::UnixListener::bind(&proxy_path).unwrap();
        let proxy_thread = std::thread::spawn(move || {
            let (mut client, _) = proxy.accept().unwrap();
            let mut server = std::os::unix::net::UnixStream::connect(socket).unwrap();
            for transaction in 0..4 {
                let mut prefix = [0; 4];
                client.read_exact(&mut prefix).unwrap();
                let n = u32::from_be_bytes(prefix) as usize;
                assert!(n <= MAX_REQUEST);
                let mut request = vec![0; n];
                client.read_exact(&mut request).unwrap();
                server.write_all(&prefix).unwrap();
                server.write_all(&request).unwrap();
                server.read_exact(&mut prefix).unwrap();
                let n = u32::from_be_bytes(prefix) as usize;
                assert!(n <= MAX_REPLY);
                let mut reply = vec![0; n];
                server.read_exact(&mut reply).unwrap();
                // Native preparation can retry for >2 seconds. A delayed
                // successful mutation must not be retried or reported lost.
                if transaction == 1 {
                    std::thread::sleep(Duration::from_secs(3));
                }
                client.write_all(&prefix).unwrap();
                client.write_all(&reply).unwrap();
            }
        });
        let output = tokio::task::spawn_blocking(move || {
            std::process::Command::new(executable)
                .arg(proxy_path)
                .output()
                .unwrap()
        })
        .await
        .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        proxy_thread.join().unwrap();
        assert_eq!(
            receive(handle.state()).await.unwrap().config.revision,
            initial.config.revision + 1
        );
        task.abort();
        let _ = task.await;
        handle.close();
        handle.shutdown().await.unwrap();
        owner_thread.join().unwrap();
    }
}
