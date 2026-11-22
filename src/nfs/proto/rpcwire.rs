// SPDX-License-Identifier: BSD-3-Clause
// From nfsserve 0.11.0 (Copyright (c) 2023, XetData; Copyright (c) 2025, Hugging Face), with
// sacdfs's changes: see mod.rs and this folder's LICENSE.

use std::io::{Cursor, Read, Write};

use log::{debug, error, trace, warn};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::context::RPCContext;
use super::rpc::*;
use super::xdr::*;
use super::{mount, mount_handlers, nfs, nfs_handlers, portmap, portmap_handlers};

// Information from RFC 5531
// https://datatracker.ietf.org/doc/html/rfc5531

const NFS_ACL_PROGRAM: u32 = 100227;
const NFS_ID_MAP_PROGRAM: u32 = 100270;
const NFS_METADATA_PROGRAM: u32 = 200024;

/// Answers one call (`input`, a whole record) into `output`: `Ok(false)` for no reply.
pub async fn handle_rpc(
    input: &mut impl Read,
    output: &mut impl Write,
    mut context: RPCContext,
) -> Result<bool, std::io::Error> {
    let mut recv = rpc_msg::default();
    recv.deserialize(input)?;
    let xid = recv.xid;
    if let rpc_body::CALL(call) = recv.body {
        let unix = if let auth_flavor::AUTH_UNIX = call.cred.flavor {
            let mut auth = auth_unix::default();
            auth.deserialize(&mut Cursor::new(&call.cred.body))?;
            context.auth = auth;
            true
        } else {
            false
        };
        // sacdfs: only callers the file system permits, but anyone may ping (procedure 0).
        if call.proc != 0 && !context.vfs.permits(unix.then_some(&context.auth)) {
            debug!("refused call {xid} (program {}, procedure {}): credentials", call.prog, call.proc);
            let stat = if unix { auth_stat::AUTH_BADCRED } else { auth_stat::AUTH_TOOWEAK };
            auth_error_reply_message(xid, stat).serialize(output)?;
            return Ok(true);
        }
        if call.rpcvers != 2 {
            warn!("Invalid RPC version {} != 2", call.rpcvers);
            rpc_vers_mismatch(xid).serialize(output)?;
            return Ok(true);
        }

        if context.transaction_tracker.is_retransmission(xid, &context.client_addr) {
            // This is a retransmission
            // Drop the message and return
            debug!(
                "Retransmission detected, xid: {}, client_addr: {}, call: {:?}",
                xid, context.client_addr, call
            );
            return Ok(false);
        }

        let res = {
            if call.prog == nfs::PROGRAM {
                nfs_handlers::handle_nfs(xid, call, input, output, &context).await
            } else if call.prog == portmap::PROGRAM {
                portmap_handlers::handle_portmap(xid, call, input, output, &context)
            } else if call.prog == mount::PROGRAM {
                mount_handlers::handle_mount(xid, call, input, output, &context).await
            } else if call.prog == NFS_ACL_PROGRAM
                || call.prog == NFS_ID_MAP_PROGRAM
                || call.prog == NFS_METADATA_PROGRAM
            {
                trace!("ignoring NFS_ACL packet");
                prog_unavail_reply_message(xid).serialize(output)?;
                Ok(())
            } else {
                warn!("Unknown RPC Program number {} != {}", call.prog, nfs::PROGRAM);
                prog_unavail_reply_message(xid).serialize(output)?;
                Ok(())
            }
        }
        .map(|_| true);
        context.transaction_tracker.mark_processed(xid, &context.client_addr);
        res
    } else {
        error!("Unexpectedly received a Reply instead of a Call");
        Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Bad RPC Call format"))
    }
}

/// sacdfs: the longest record read, in bytes. A longer one ends the connection.
pub const MAX_RECORD: usize = 256 * 1024;

/// Reads one record into `buf` (cleared first): `Ok(false)` at the end of the stream before
/// a record starts.
///
/// RFC 5531 section 11, record marking: a record is fragments, each a 4-byte header (the top
/// bit set on the last, the low 31 bits its length) and its bytes.
///
/// sacdfs: replaces nfsserve's reader, which allocated whatever length a header said (up to
/// 2 GiB) before reading it. A record longer than [`MAX_RECORD`] is an error.
pub async fn read_record(src: &mut (impl AsyncRead + Unpin), buf: &mut Vec<u8>) -> std::io::Result<bool> {
    buf.clear();
    loop {
        let mut header = [0u8; 4];
        match src.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof && buf.is_empty() => return Ok(false),
            Err(e) => return Err(e),
        }
        let header = u32::from_be_bytes(header);
        let last = header & (1 << 31) != 0;
        let length = (header & !(1 << 31)) as usize;
        if buf.len() + length > MAX_RECORD {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("record longer than {MAX_RECORD} bytes"),
            ));
        }
        trace!("Reading fragment length:{}, last:{}", length, last);
        let start = buf.len();
        src.take(length as u64).read_to_end(buf).await?;
        if buf.len() - start != length {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        if last {
            return Ok(true);
        }
    }
}

/// Writes `buf` as one record of one fragment.
pub async fn write_fragment(dest: &mut (impl AsyncWrite + Unpin), buf: &[u8]) -> std::io::Result<()> {
    let length = u32::try_from(buf.len())
        .ok()
        .filter(|l| *l < 1 << 31)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "reply too long"))?;
    let mut out = Vec::with_capacity(buf.len() + 4);
    out.extend_from_slice(&(length | 1 << 31).to_be_bytes());
    out.extend_from_slice(buf);
    trace!("Writing fragment length:{}", buf.len());
    dest.write_all(&out).await
}
