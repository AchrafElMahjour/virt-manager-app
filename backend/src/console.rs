//! Incrément 5 — relais WebSocket ⇄ VNC, authentifié et restreint au port VNC libvirt en boucle locale.
//!
//! Le rôle (`operator` minimum) et l'origine du navigateur sont vérifiés par `auth::guard` avant
//! l'ouverture du WebSocket. Le client ne fournit jamais de port : il est lu dans le XML libvirt.
//! Hyperviseur distant `qemu+ssh://` : le port VNC (qui écoute sur 127.0.0.1 *de l'hôte distant*)
//! est atteint par un tunnel `ssh -W`, sans jamais ouvrir de port sur le réseau.
use crate::{
    api_error,
    hypervisors::{parse_libvirt_uri, AppState},
    libvirt_error, ApiErr, ApiResult,
};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path,
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;
use std::process::Stdio;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    process::Command,
};
use utoipa::ToSchema;
use virt::domain::Domain;

#[derive(Serialize, ToSchema)]
pub struct ConsoleInfo {
    pub graphics: String,
    pub available: bool,
    pub message: String,
    pub websocket_path: Option<String>,
}

fn vnc_port(xml: &str) -> Result<(String, Option<u16>), ApiErr> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| libvirt_error(e.to_string()))?;
    let graphics = doc.descendants().find(|n| n.has_tag_name("graphics"));
    let Some(graphics) = graphics else {
        return Ok(("none".into(), None));
    };
    let kind = graphics.attribute("type").unwrap_or("unknown").to_string();
    if kind != "vnc" {
        return Ok((kind, None));
    }
    // Do not proxy to a VNC port exposed on the LAN or to an arbitrary host.
    let addr = graphics.attribute("listen").or_else(|| {
        graphics
            .children()
            .find(|n| n.has_tag_name("listen"))
            .and_then(|n| n.attribute("address"))
    });
    if addr != Some("127.0.0.1") {
        return Ok((kind, None));
    }
    let port = graphics
        .attribute("port")
        .and_then(|s| s.parse::<u16>().ok());
    Ok((kind, port.filter(|p| *p >= 5900)))
}
fn active_vnc(app: &AppState, id: &str) -> Result<(String, Option<u16>), ApiErr> {
    let conn = app.open_ro().map_err(libvirt_error)?;
    let dom = Domain::lookup_by_uuid_string(&conn, id)
        .map_err(|_| api_error(StatusCode::NOT_FOUND, "VM introuvable"))?;
    if dom
        .get_info()
        .map_err(|e| libvirt_error(e.to_string()))?
        .state
        != 1
    {
        return Ok(("inactive".into(), None));
    }
    let xml = dom
        .get_xml_desc(0)
        .map_err(|e| libvirt_error(e.to_string()))?;
    vnc_port(&xml)
}

#[utoipa::path(get, path="/api/v1/vms/{uuid}/console", params(("uuid"=String,Path)),
    responses((status=200,body=ConsoleInfo),(status=404,body=crate::ApiError),(status=502,body=crate::ApiError)),tag="Console")]
pub async fn console_info(app: AppState, Path(raw): Path<String>) -> ApiResult<ConsoleInfo> {
    let id = uuid::Uuid::parse_str(&raw)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "UUID invalide"))?
        .to_string();
    let iid = id.clone();
    let remote_tls = app.libvirt_uri.starts_with("qemu+tls://");
    let (kind, port) = tokio::task::spawn_blocking(move || active_vnc(&app, &iid))
        .await
        .map_err(|e| libvirt_error(e.to_string()))??;
    let port = port.filter(|_| !remote_tls);
    let message=match (kind.as_str(),port) {
        ("vnc",Some(_))=>"Console VNC locale disponible via WebSocket/noVNC.",
        ("spice",_)=>"Cette VM utilise SPICE : ouvrez sa console dans virt-manager/remote-viewer. Conversion automatique désactivée.",
        ("inactive",_)=>"Démarrez la VM pour ouvrir sa console.",
        ("vnc",None) if remote_tls=>"Console web indisponible sur un hyperviseur qemu+tls:// (seul qemu+ssh:// permet le tunnel). Utilisez virt-manager.",
        ("vnc",_)=>"La console VNC doit écouter sur 127.0.0.1 avec un port actif.",
        _=>"Aucune console VNC prise en charge n'est configurée.",
    }.to_string();
    Ok(Json(ConsoleInfo {
        graphics: kind,
        available: port.is_some(),
        message,
        websocket_path: port.map(|_| format!("/api/v1/vms/{id}/vnc")),
    }))
}

/// Destination du tunnel SSH vers un hyperviseur `qemu+ssh://` (valeurs déjà validées à l'enregistrement).
fn ssh_command(uri: &str, port: u16) -> Result<Command, ApiErr> {
    let p = parse_libvirt_uri(uri).map_err(libvirt_error)?;
    let host = p
        .host
        .ok_or_else(|| libvirt_error("URI sans hôte".to_string()))?;
    let destination = match &p.user {
        Some(u) => format!("{u}@{host}"),
        None => host,
    };
    let mut cmd = Command::new("ssh");
    cmd.args(["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10"]);
    if let Some(sp) = p.port {
        cmd.args(["-p", &sp.to_string()]);
    }
    if let Some((_, key)) = p.params.iter().find(|(k, _)| k == "keyfile") {
        cmd.args(["-i", key]);
    }
    cmd.args(["-W", &format!("127.0.0.1:{port}"), "--", &destination]);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    Ok(cmd)
}

pub async fn websocket_vnc(
    app: AppState,
    Path(raw): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiErr> {
    let id = uuid::Uuid::parse_str(&raw)
        .map_err(|_| api_error(StatusCode::BAD_REQUEST, "UUID invalide"))?
        .to_string();
    let uri = app.libvirt_uri.clone();
    let (kind, port) = {
        let app = app.clone();
        tokio::task::spawn_blocking(move || active_vnc(&app, &id))
            .await
            .map_err(|e| libvirt_error(e.to_string()))??
    };
    let port = port.ok_or_else(|| {
        api_error(
            StatusCode::CONFLICT,
            format!("Console VNC locale indisponible : {kind}"),
        )
    })?;
    if uri.starts_with("qemu+ssh://") {
        let mut child = ssh_command(&uri, port)?
            .spawn()
            .map_err(|e| libvirt_error(format!("Tunnel SSH impossible : {e}")))?;
        let rd = child
            .stdout
            .take()
            .ok_or_else(|| libvirt_error("Tunnel SSH sans sortie".into()))?;
        let wr = child
            .stdin
            .take()
            .ok_or_else(|| libvirt_error("Tunnel SSH sans entrée".into()))?;
        return Ok(ws
            .protocols(["binary"])
            .on_upgrade(move |socket| async move {
                proxy_vnc(socket, rd, wr).await;
                drop(child); // kill_on_drop : le tunnel ssh ne survit pas à la session
            })
            .into_response());
    }
    if uri.starts_with("qemu+tls://") {
        return Err(api_error(
            StatusCode::CONFLICT,
            "Console web indisponible sur un hyperviseur qemu+tls://",
        ));
    }
    let tcp = TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| libvirt_error(format!("Connexion VNC locale refusée : {e}")))?;
    let (rd, wr) = tcp.into_split();
    Ok(ws
        .protocols(["binary"])
        .on_upgrade(move |socket| proxy_vnc(socket, rd, wr))
        .into_response())
}

async fn proxy_vnc<R, W>(mut ws: WebSocket, mut rd: R, mut wr: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buf = [0_u8; 64 * 1024];
    loop {
        tokio::select! {
            read = rd.read(&mut buf) => match read {
                Ok(0) | Err(_) => break,
                Ok(n) => if ws.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() { break; },
            },
            msg = ws.recv() => match msg {
                Some(Ok(Message::Binary(data))) => if wr.write_all(&data).await.is_err() { break; },
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {},
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepts_only_local_vnc_port() {
        assert_eq!(vnc_port("<domain><devices><graphics type='vnc' listen='127.0.0.1' port='5907'/></devices></domain>").unwrap(),("vnc".into(),Some(5907)));
        assert_eq!(vnc_port("<domain><devices><graphics type='vnc' listen='0.0.0.0' port='5907'/></devices></domain>").unwrap().1,None);
        assert_eq!(
            vnc_port("<domain><devices><graphics type='spice' port='5907'/></devices></domain>")
                .unwrap()
                .1,
            None
        );
    }

    #[test]
    fn ssh_tunnel_command_is_built_from_validated_uri_only() {
        let cmd = ssh_command(
            "qemu+ssh://bob@10.0.0.5:2222/system?keyfile=/home/bob/.ssh/id&no_tty=1",
            5901,
        )
        .unwrap();
        let args: Vec<String> = cmd
            .as_std()
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let joined = args.join(" ");
        assert!(joined.contains("-p 2222") && joined.contains("-i /home/bob/.ssh/id"));
        assert!(joined.contains("-W 127.0.0.1:5901 -- bob@10.0.0.5"));
        assert!(joined.contains("BatchMode=yes"));
    }
}
