//! Gliff lock-screen input over omarchy-launchpad's authenticated SSH connection.
use crate::{config::Host, remote::Task};
use gliff_proto::{
    frame::{HEADER_LEN, MAX_FRAME_BODY, MAX_PAYLOAD},
    msg::{ChromaMode, ClientCaps, ClientMsg, Codec, PROTOCOL_VERSION, ServerMsg},
};
use std::{
    io::{self, BufReader, BufWriter, Read, Write},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_SIZE: (u32, u32) = (1280, 720);
const WAKE_DISPLAY: &str = "if command -v omarchy-brightness-display >/dev/null; then omarchy-brightness-display on >/dev/null; fi";

// Both operations select the same Hyprland instance as the user's desktop.
const HYPRLAND: &str = r#"
signature=$(systemctl --user show-environment | sed -n 's/^HYPRLAND_INSTANCE_SIGNATURE=//p')
if [ -n "$signature" ]; then export HYPRLAND_INSTANCE_SIGNATURE="$signature"; fi
"#;

pub fn probe(host: &Host, wake: bool) -> Result<Task<Option<bool>>, String> {
    let mut cmd = host.ssh_command();
    let before_check = if wake {
        format!(
            "if hyprctl -j locked | grep -Eq '\"locked\"[[:space:]]*:[[:space:]]*true'; then {WAKE_DISPLAY}; fi"
        )
    } else {
        String::new()
    };
    cmd.arg(format!(
        "if ! command -v hyprctl >/dev/null; then printf 'unsupported\\n'; exit; fi\n{HYPRLAND}\n{before_check}\nexec hyprctl -j locked"
    ));
    Task::spawn(cmd, |stdout, _| {
        let mut reply = String::new();
        stdout.take(4096).read_to_string(&mut reply)?;
        if reply.trim() == "unsupported" {
            return Ok(None);
        }
        let state: serde_json::Value = serde_json::from_str(&reply)?;
        state["locked"]
            .as_bool()
            .map(Some)
            .ok_or_else(|| io::Error::other("Hyprland did not report lock state."))
    })
}

pub fn lock(host: &Host) -> Result<Task<()>, String> {
    let mut cmd = host.ssh_command();
    cmd.arg(format!(
        "{HYPRLAND}\n{}",
        r#"
locker=$(command -v omarchy-system-lock) ||
    { echo 'Omarchy’s lock command is unavailable on this host.' >&2; exit 1; }
systemd-run --user --quiet --wait --pipe --collect "$locker" || exit
for attempt in 1 2 3 4 5 6 7 8 9 10; do
    if hyprctl -j locked | grep -Eq '"locked"[[:space:]]*:[[:space:]]*true'; then exit 0; fi
    sleep 0.3
done
echo 'Omarchy did not confirm the lock.' >&2
exit 1
"#,
    ));
    Task::spawn(cmd, |mut stdout, _| {
        io::copy(&mut stdout, &mut io::sink()).map(|_| ())
    })
}

#[derive(Clone, Copy)]
pub struct Geometry {
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

pub struct Mirror {
    task: Task<()>,
    latest: Arc<Mutex<Option<Geometry>>>,
    writer: mpsc::SyncSender<Vec<ClientMsg>>,
}

impl Mirror {
    pub fn start(host: &Host) -> Result<Self, String> {
        let mut cmd = host.ssh_command();
        cmd.arg(format!(
            "{HYPRLAND}\n{}\n{WAKE_DISPLAY}\n{}",
            r#"
command -v gliff-server >/dev/null || { echo 'Install gliff on Omarchy to unlock remotely.' >&2; exit 1; }
state=$(hyprctl -j locked) || exit
printf '%s\n' "$state" | grep -Eq '"locked"[[:space:]]*:[[:space:]]*true' ||
    { echo 'Omarchy is already unlocked.' >&2; exit 1; }
"#,
            r#"
set -- gliff-server --stdio --low-bandwidth
if [ -n "$HYPRLAND_INSTANCE_SIGNATURE" ]; then set -- "$@" --instance "$HYPRLAND_INSTANCE_SIGNATURE"; fi
exec "$@"
"#,
        ));
        let latest = Arc::new(Mutex::new(None));
        let frames = latest.clone();
        let (writer, outbound) = mpsc::sync_channel::<Vec<ClientMsg>>(64);
        let replies = writer.clone();
        let task = Task::spawn(cmd, move |stdout, stdin| {
            let mut output = BufWriter::new(stdin);
            write_message(
                &mut output,
                &ClientMsg::Hello {
                    version: PROTOCOL_VERSION,
                    keymap: String::new(), // Gliff's US layout, with characters mapped locally.
                    caps: ClientCaps {
                        codecs: vec![Codec::H264],
                        max_width: MAX_SIZE.0,
                        max_height: MAX_SIZE.1,
                        chroma: vec![ChromaMode::Single420],
                        features: vec![],
                    },
                },
            )?;
            thread::spawn(move || {
                while let Ok(batch) = outbound.recv() {
                    for message in batch {
                        if write_message(&mut output, &message).is_err() {
                            return;
                        }
                    }
                }
            });
            receive(BufReader::new(stdout), &frames, &replies)
        })?;
        Ok(Self {
            task,
            latest,
            writer,
        })
    }

    pub fn poll(&self) -> Result<Option<Geometry>, String> {
        if let Some(result) = self.task.poll() {
            return result.and(Err("Gliff disconnected.".into()));
        }
        Ok(self.latest.lock().unwrap().take())
    }

    pub fn send(&self, messages: Vec<ClientMsg>) -> Result<(), String> {
        self.writer
            .try_send(messages)
            .map_err(|_| "Could not send input to Omarchy. Reopen the unlock window.".into())
    }
}

fn write_message(output: &mut impl Write, message: &ClientMsg) -> io::Result<()> {
    let body = minicbor::to_vec(message).map_err(io::Error::other)?;
    output.write_all(&(body.len() as u32).to_le_bytes())?;
    output.write_all(&0u32.to_le_bytes())?;
    output.write_all(&body)?;
    output.flush()
}

fn receive(
    mut input: impl Read,
    latest: &Mutex<Option<Geometry>>,
    writer: &mpsc::SyncSender<Vec<ClientMsg>>,
) -> io::Result<()> {
    let mut accepted = false;
    let mut stream: Option<Geometry> = None;
    let mut body = Vec::new();
    loop {
        let mut header = [0; HEADER_LEN];
        input.read_exact(&mut header)?;
        let body_len = u32::from_le_bytes(header[..4].try_into().unwrap()) as usize;
        let payload_len = u32::from_le_bytes(header[4..].try_into().unwrap());
        if body_len > MAX_FRAME_BODY || payload_len > MAX_PAYLOAD {
            return Err(io::Error::other(
                "Gliff sent a frame larger than the limit.",
            ));
        }
        body.resize(body_len, 0);
        input.read_exact(&mut body)?;
        let mut payload = input.by_ref().take(payload_len.into());
        match minicbor::decode::<ServerMsg>(&body) {
            Ok(ServerMsg::HelloAck {
                version, session, ..
            }) => {
                if accepted || version != PROTOCOL_VERSION || session.headless {
                    return Err(io::Error::other("Gliff did not start a compatible mirror."));
                }
                accepted = true;
            }
            Ok(ServerMsg::Ping { t }) if accepted => {
                reply(writer, ClientMsg::Pong { t })?;
            }
            Ok(ServerMsg::StreamConfig {
                codec,
                chroma,
                width,
                height,
                scale_milli,
                ..
            }) if accepted => {
                if codec != Codec::H264
                    || chroma != ChromaMode::Single420
                    || !(1..=MAX_SIZE.0).contains(&width)
                    || !(1..=MAX_SIZE.1).contains(&height)
                    || scale_milli == 0
                {
                    return Err(io::Error::other("Unsupported Gliff video configuration."));
                }
                stream = Some(Geometry {
                    width,
                    height,
                    scale: f64::from(scale_milli) / 1000.0,
                });
            }
            Ok(ServerMsg::VideoFrame {
                frame_id,
                data_len,
                aux_len,
                ..
            }) => {
                let geometry = stream
                    .ok_or_else(|| io::Error::other("Gliff sent video before configuration."))?;
                if aux_len != 0 || data_len != payload_len || data_len == 0 {
                    return Err(io::Error::other("Invalid Gliff video payload."));
                }
                discard_payload(&mut payload)?;
                // Only the output geometry is needed to focus the remote password field.
                *latest.lock().unwrap() = Some(geometry);
                reply(
                    writer,
                    ClientMsg::FrameAck {
                        frame_id,
                        decoded_at_ms: SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64,
                    },
                )?;
            }
            Ok(ServerMsg::Error { message, .. }) => return Err(io::Error::other(message)),
            _ if !accepted => {
                return Err(io::Error::other("Gliff did not acknowledge the mirror."));
            }
            Err(error) if !error.is_unknown_variant() => return Err(io::Error::other(error)),
            _ => {} // Cursor and clipboard payloads are discarded.
        }
        discard_payload(&mut payload)?;
    }
}

fn discard_payload(payload: &mut io::Take<impl Read>) -> io::Result<()> {
    io::copy(payload, &mut io::sink())?;
    if payload.limit() != 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Incomplete Gliff payload.",
        ));
    }
    Ok(())
}

fn reply(writer: &mpsc::SyncSender<Vec<ClientMsg>>, message: ClientMsg) -> io::Result<()> {
    writer
        .try_send(vec![message])
        .map_err(|_| io::Error::other("Gliff control queue closed or full."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gliff_proto::msg::SessionInfo;

    fn append(wire: &mut Vec<u8>, message: ServerMsg, payload: &[u8]) {
        let body = minicbor::to_vec(message).unwrap();
        wire.extend_from_slice(&(body.len() as u32).to_le_bytes());
        wire.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        wire.extend(body);
        wire.extend_from_slice(payload);
    }

    fn config(fps_cap: u32) -> ServerMsg {
        ServerMsg::StreamConfig {
            codec: Codec::H264,
            chroma: ChromaMode::Single420,
            pipeline: None,
            width: 64,
            height: 64,
            scale_milli: 1000,
            extradata: vec![],
            aux_extradata: None,
            view_width: 64,
            view_height: 64,
            fps_cap,
        }
    }

    fn video(wire: &mut Vec<u8>, id: u64, keyframe: bool) {
        append(
            wire,
            ServerMsg::VideoFrame {
                frame_id: id,
                pts_us: 0,
                keyframe,
                damage: vec![],
                data_len: 3,
                aux_len: 0,
            },
            &[1, 2, 3],
        );
    }

    #[test]
    fn acknowledges_video_without_decoding_or_retaining_pixels() {
        let mut wire = Vec::new();
        append(
            &mut wire,
            ServerMsg::HelloAck {
                version: PROTOCOL_VERSION,
                session: SessionInfo {
                    headless: false,
                    output: "test".into(),
                },
                outputs: vec![],
                features: vec![],
            },
            &[],
        );
        append(&mut wire, config(30), &[]);
        video(&mut wire, 1, true);
        append(&mut wire, config(24), &[]);
        video(&mut wire, 2, false);

        let latest = Mutex::new(None);
        let (writer, replies) = mpsc::sync_channel(8);
        let error = receive(wire.as_slice(), &latest, &writer).unwrap_err();
        let geometry = latest.into_inner().unwrap().unwrap();
        assert_eq!(
            (geometry.width, geometry.height, geometry.scale),
            (64, 64, 1.)
        );
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof, "{error}");
        assert!(
            replies
                .try_iter()
                .flatten()
                .any(|reply| matches!(reply, ClientMsg::FrameAck { frame_id: 2, .. }))
        );
    }
}
