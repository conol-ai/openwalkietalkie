//! Bench only (`panel-debug`): `dbg.panel` on the host request channel.
//! See `display::diag` for what each operation does.
//!
//! `{"method":"dbg.panel","params":{"op":"read","r":4,"n":3}}` and so on:
//!   * `read` `r` register, `n` bytes (1-4), `line` LCDC SPI line mode
//!     (default 0 = 4-line single data), `dummy` cycles, `div` clock
//!     divider (0 = write clock / 25), `t` 1 = data-type access;
//!   * `cmd` `r` register, `n` parameter bytes taken from `v`, first byte
//!     in the top of the `n`-byte number;
//!   * `te` `ms`; `init` `low` reset-low milliseconds; `regs`.
//!
//! The answer is `{"result":{"v":[...]}}` or `{"error":{"code":E}}` with
//! E = 1 no panel, 2 LCDC timeout, 3 bad parameter, 4 app task did not answer.
use super::wire::{as_str, find_key, parse_u32, Out};
use crate::display::diag::{self, Op};

pub async fn handle(req: &[u8], tx: &mut [u8]) -> Option<usize> {
    if find_key(req, b"method").and_then(as_str)? != b"dbg.panel" {
        return None;
    }
    let params = find_key(req, b"params");
    let arg = |key: &[u8]| params.and_then(|p| find_key(p, key)).and_then(parse_u32);
    let byte = |key: &[u8], default: u32| arg(key).unwrap_or(default).min(255) as u8;
    let op = match params.and_then(|p| find_key(p, b"op")).and_then(as_str) {
        Some(b"read") => Some(Op::Read {
            reg: byte(b"r", 0x04),
            len: byte(b"n", 1),
            line: byte(b"line", 0),
            dummy: byte(b"dummy", 0),
            div: byte(b"div", 0),
            data: arg(b"t").unwrap_or(0) != 0,
        }),
        Some(b"cmd") => {
            let n = byte(b"n", 0).min(4);
            let v = arg(b"v").unwrap_or(0);
            let mut params = [0u8; 4];
            for (i, p) in params.iter_mut().enumerate().take(usize::from(n)) {
                *p = (v >> (8 * (usize::from(n) - 1 - i))) as u8;
            }
            Some(Op::Cmd { reg: byte(b"r", 0), params, n })
        }
        Some(b"te") => Some(Op::Te { ms: arg(b"ms").unwrap_or(100) }),
        Some(b"init") => Some(Op::Init { low_ms: arg(b"low").unwrap_or(10) }),
        Some(b"regs") => Some(Op::Regs),
        _ => None,
    };
    let mut out = Out::new(tx);
    out.push(b"{\"id\":");
    match find_key(req, b"id") {
        Some(id) if id.len() <= 40 => out.push(id),
        _ => out.push(b"null"),
    }
    let reply = match op {
        Some(op) => diag::request(op).await.unwrap_or(diag::Reply { err: 4, ..Default::default() }),
        None => diag::Reply { err: 3, ..Default::default() },
    };
    if reply.err != 0 {
        out.push(b",\"error\":{\"code\":");
        out.push_u32(u32::from(reply.err));
        out.push(b"}}");
    } else {
        out.push(b",\"result\":{\"v\":[");
        for (i, v) in reply.vals[..usize::from(reply.n)].iter().enumerate() {
            if i > 0 {
                out.push(b",");
            }
            out.push_u32(*v);
        }
        out.push(b"]}}");
    }
    Some(out.finish())
}
