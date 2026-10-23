//! Single-stepping PCSX-Redux through its GDB remote server, for lockstep
//! comparison with the native core.
//!
//! Redux reports 38 registers in `g`: r0..r31, SR, LO, HI, BadVAddr, Cause, PC.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::{Backend, ReduxError, Result, io};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Regs {
    pub gpr: [u32; 32],
    pub sr: u32,
    pub lo: u32,
    pub hi: u32,
    pub badvaddr: u32,
    pub cause: u32,
    pub pc: u32,
}

pub struct GdbSession {
    child: Child,
    stream: BufReader<TcpStream>,
    pub dir: PathBuf,
}

fn checksum(data: &str) -> u8 {
    data.bytes().fold(0u8, |a, b| a.wrapping_add(b))
}

fn free_port() -> Result<u16> {
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| ReduxError::Setup(format!("no free port: {e}")))?;
    Ok(l.local_addr().map_err(|e| ReduxError::Setup(e.to_string()))?.port())
}

impl GdbSession {
    /// Start Redux paused at the BIOS reset vector with the GDB server enabled.
    pub fn start(backend: &Backend, bios: &Path, dir: &Path) -> Result<Self> {
        io(dir, std::fs::create_dir_all(dir.join("config")))?;
        let dir = io(dir, dir.canonicalize())?;
        let bios = io(bios, bios.canonicalize())?;
        let port = free_port()?;
        let cfg = dir.join("config/pcsx.json");
        io(&cfg, std::fs::write(&cfg, "{\"SPU\":{\"NullSync\":true,\"Speed\":0}}\n"))?;
        let log_path = dir.join("redux.log");
        let log = io(&log_path, std::fs::File::create(&log_path))?;
        let log_err = io(&log_path, log.try_clone())?;
        let child = Command::new(&backend.binary)
            .current_dir(&dir)
            .arg("-no-ui")
            .arg("-portable")
            .arg(dir.join("config"))
            .arg("-bios")
            .arg(&bios)
            .arg("-interpreter")
            .arg("-debugger")
            .arg("-gdb")
            .arg("-gdb-port")
            .arg(port.to_string())
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_err)
            .spawn()
            .map_err(|e| ReduxError::Io(backend.binary.clone(), e))?;
        let deadline = Instant::now() + Duration::from_secs(20);
        let stream = loop {
            match TcpStream::connect(("127.0.0.1", port)) {
                Ok(s) => break s,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
                Err(e) => return Err(ReduxError::Setup(format!("GDB server did not open port {port}: {e}"))),
            }
        };
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(Duration::from_secs(30))).ok();
        let mut s = GdbSession { child, stream: BufReader::new(stream), dir };
        s.command("QStartNoAckMode")?;
        Ok(s)
    }

    fn send(&mut self, data: &str) -> Result<()> {
        let pkt = format!("${data}#{:02x}", checksum(data));
        self.stream.get_mut().write_all(pkt.as_bytes()).map_err(|e| ReduxError::Setup(format!("gdb write: {e}")))
    }

    fn recv(&mut self) -> Result<String> {
        let err = |e: std::io::Error| ReduxError::Setup(format!("gdb read: {e}"));
        loop {
            let mut b = [0u8; 1];
            self.stream.read_exact(&mut b).map_err(err)?;
            match b[0] {
                b'+' | b'-' => continue,
                b'$' => break,
                _ => continue,
            }
        }
        let mut body = Vec::new();
        self.stream.read_until(b'#', &mut body).map_err(err)?;
        body.pop();
        let mut cs = [0u8; 2];
        self.stream.read_exact(&mut cs).map_err(err)?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// Send a packet and return the reply; acknowledges in ack mode.
    pub fn command(&mut self, data: &str) -> Result<String> {
        self.send(data)?;
        let reply = self.recv()?;
        if data == "QStartNoAckMode" {
            let _ = self.stream.get_mut().write_all(b"+");
        }
        Ok(reply)
    }

    pub fn registers(&mut self) -> Result<Regs> {
        let hex = self.command("g")?;
        let word = |i: usize| -> Result<u32> {
            let s = hex.get(i * 8..i * 8 + 8).ok_or_else(|| ReduxError::Setup(format!("short g reply: {hex}")))?;
            let v = u32::from_str_radix(s, 16).map_err(|_| ReduxError::Setup(format!("bad g reply: {s}")))?;
            Ok(v.swap_bytes())
        };
        let mut r = Regs::default();
        for (i, g) in r.gpr.iter_mut().enumerate() {
            *g = word(i)?;
        }
        r.sr = word(32)?;
        r.lo = word(33)?;
        r.hi = word(34)?;
        r.badvaddr = word(35)?;
        r.cause = word(36)?;
        r.pc = word(37)?;
        Ok(r)
    }

    /// Execute one instruction; returns the stop reply.
    pub fn step(&mut self) -> Result<String> {
        self.command("s")
    }

    pub fn read_memory(&mut self, addr: u32, len: u32) -> Result<Vec<u8>> {
        let hex = self.command(&format!("m{addr:x},{len:x}"))?;
        Ok((0..hex.len() / 2).filter_map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).ok()).collect())
    }
}

impl Drop for GdbSession {
    fn drop(&mut self) {
        let _ = self.send("k");
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
