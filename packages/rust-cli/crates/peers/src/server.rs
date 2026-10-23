//! 收件箱的服务（docs/specs/rust-cli/cross-session.md §2.1）：每个连接读一行帧，交给绑定的会话（入站表），会话提交
//! 之后回一行 `accepted`。连接 30 s 内没有完整的一行就关闭（传输层的超时，不用来同步状态）。

use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::Spawner;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::peers::{
    InboundPeerMessage, PeerAddress, PeerReceipt, PeerSignal, ReceiptStatus,
};
use rust_cli_net::uds::{UdsListener, UdsStream};

use crate::frames::{Frame, Reply, line};
use crate::inbox::Inbox;

/// 一个连接等第一行的上限。
const FIRST_LINE: Duration = Duration::from_secs(30);

pub(crate) async fn serve(
    listener: UdsListener,
    inbox: Arc<Inbox>,
    clock: Arc<dyn Clock>,
    spawner: Spawner,
) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let (inbox, clock) = (inbox.clone(), clock.clone());
                spawner.spawn("peer-connection", handle(stream, inbox, clock));
            }
            Err(error) => {
                tracing::warn!(%error, "the cross-session inbox stopped accepting");
                return;
            }
        }
    }
}

async fn handle(mut stream: UdsStream, inbox: Arc<Inbox>, clock: Arc<dyn Clock>) {
    let Ok(Ok(Some(text))) = tokio::time::timeout(FIRST_LINE, stream.read_line()).await else {
        return;
    };
    let reply = match serde_json::from_str::<Frame>(&text) {
        Ok(frame) => receive(frame, &inbox, clock.as_ref()).await,
        Err(_) => Reply::Refused {
            reason: "not a frame".to_owned(),
        },
    };
    if let Err(error) = stream.write_line(&line(&reply)).await {
        tracing::debug!(%error, "the peer closed before the reply");
    }
}

async fn receive(frame: Frame, inbox: &Inbox, clock: &dyn Clock) -> Reply {
    let wait = match frame {
        Frame::User {
            msg_id,
            from,
            message,
            ..
        } => {
            // 没有内容的消息不收；回执要发回 `from`，地址不是 `uds:` 形状的不收。
            let from = from.as_deref().and_then(PeerAddress::parse);
            let (Some(from), false) = (from, message.content.is_empty()) else {
                return Reply::Refused {
                    reason: "a message needs content and a uds: sender".to_owned(),
                };
            };
            let received_at_ms = clock.now_ms();
            inbox.push(|seq| {
                PeerSignal::Message(InboundPeerMessage {
                    seq,
                    msg_id,
                    from,
                    envelope: message.content,
                    received_at_ms,
                })
            })
        }
        Frame::Control {
            action,
            orig_msg_id,
            status,
        } if action == "peer_message_status" => {
            let status = status.as_deref().and_then(ReceiptStatus::parse);
            let (Some(msg_id), Some(status)) = (orig_msg_id, status) else {
                return Reply::Ignored;
            };
            let Some(recipient) = inbox.match_receipt(&msg_id, status) else {
                tracing::debug!(%msg_id, "a receipt for no outstanding send was dropped");
                return Reply::Ignored;
            };
            inbox.push(|seq| {
                PeerSignal::Receipt(PeerReceipt {
                    seq,
                    msg_id,
                    status,
                    recipient,
                })
            })
        }
        Frame::Control { .. } => return Reply::Ignored,
    };
    let Some(wait) = wait else {
        return Reply::Refused {
            reason: "the session is shutting down".to_owned(),
        };
    };
    match wait.await {
        Ok(()) => Reply::Accepted,
        Err(_) => Reply::Refused {
            reason: "the session is shutting down".to_owned(),
        },
    }
}
