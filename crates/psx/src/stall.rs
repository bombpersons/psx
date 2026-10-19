//! Run warnings: a run that reached its frame count but stopped drawing.
//!
//! A game stuck in a load loop keeps taking vblanks and running code, so the
//! run "completes" with a black or frozen screen. These checks look at the
//! end of the run and say so, with the CD read the game keeps retrying when
//! there is one.

use psx_core::Machine;
use serde_json::{json, Value};

/// Frames without a GPU draw before a run is flagged (2 s at NTSC).
const IDLE_FRAMES: u64 = 120;
/// Setloc to the same position this many times while idle counts as a retry loop.
const LOOP_SETLOCS: u32 = 3;

/// Warnings for the machine's state at the end of a run. Draws are tracked
/// from power-on or the state the run restored.
pub fn warnings(m: &Machine) -> Vec<Value> {
    let end = m.bus.frames;
    let since = m.bus.last_draw_frame;
    let idle = end.saturating_sub(since);
    if idle < IDLE_FRAMES {
        return Vec::new();
    }
    let cd_loop = cd_retry_loop(m, since);
    let (_, _, rgba) = m.bus.gpu.display_rgba();
    let blank = rgba.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 0]);
    let mut message = format!("no GPU draw commands in the last {idle} frames (since frame {since}), so the displayed frame has not changed");
    if let Some(l) = &cd_loop {
        message.push_str(&format!(
            "; the CPU keeps seeking the CD to LBA {}{} ({} Setlocs, about every {} frames): this looks like a load stuck in a retry loop, \
             not a finished run",
            l["lba"],
            l["owner"].as_str().map(|o| format!(" ({o})")).unwrap_or_default(),
            l["setlocs"],
            l["period_frames"],
        ));
    } else if blank {
        message.push_str("; the display is blank, so the game may be hung (check final_pc / hot PCs and the CD command history)");
    } else {
        message.push_str("; expected on a still title, menu or loading screen, suspicious if the game should be animating");
    }
    vec![json!({
        "code": "no_gpu_draws",
        "message": message,
        "frames_without_draws": idle,
        "screen_blank": blank,
        "last_draw_frame": since,
        "cd_retry_loop": cd_loop,
    })]
}

/// The CD position Setloc targeted most often since `since`, when it repeats.
fn cd_retry_loop(m: &Machine, since: u64) -> Option<Value> {
    // (params, Setlocs, first frame, last frame)
    let mut targets: Vec<(&[u8], u32, u64, u64)> = Vec::new();
    let mut commands: Vec<String> = Vec::new();
    let mut errors = 0u32;
    for r in m.bus.cdrom.recent.iter().filter(|r| r.last_frame.is_some_and(|f| f >= since)) {
        errors += r.ints.iter().filter(|&&i| i == 5).count() as u32 * r.repeats;
        let name = format!("{:02X}", r.command);
        if !commands.contains(&name) {
            commands.push(name);
        }
        if r.command != 0x02 || r.params.len() < 3 {
            continue;
        }
        let (first, last) = (r.frame.unwrap_or(0), r.last_frame.unwrap_or(0));
        match targets.iter_mut().find(|t| t.0 == r.params.as_slice()) {
            Some(t) => {
                t.1 += r.repeats;
                t.3 = last;
            }
            None => targets.push((&r.params, r.repeats, first, last)),
        }
    }
    let &(p, setlocs, first, last) = targets.iter().max_by_key(|t| t.1)?;
    if setlocs < LOOP_SETLOCS {
        return None;
    }
    let bcd = |v: u8| (v >> 4) as u32 * 10 + (v & 15) as u32;
    let lba = ((bcd(p[0]) * 60 + bcd(p[1])) * 75 + bcd(p[2])).saturating_sub(150);
    let owner = m.bus.cdrom.disc.as_ref().and_then(|d| {
        let fs = psx_media::iso9660::FileSystem::read(d).ok()?;
        fs.owner_of(lba).map(|e| e.path.clone())
    });
    Some(json!({
        "lba": lba,
        "msf": format!("{:02X}:{:02X}:{:02X}", p[0], p[1], p[2]),
        "owner": owner,
        "setlocs": setlocs,
        "first_frame": first,
        "last_frame": last,
        "period_frames": (last - first) / (setlocs as u64 - 1).max(1),
        "commands_in_loop": commands,
        "error_responses": errors,
        "note": "From the controller's last 64 distinct commands; see cdrom.command_history (psx native) or --trace-cd for the full sequence.",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use psx_core::cdrom::RecentCommand;

    fn cmd(frame: u64, command: u8, params: &[u8]) -> RecentCommand {
        RecentCommand { time: 0, frame: Some(frame), last_time: 0, last_frame: Some(frame), command, params: params.to_vec(), ints: vec![3], repeats: 1 }
    }

    #[test]
    fn idle_run_with_repeated_setloc_reports_the_retry_loop() {
        let mut m = Machine::new(vec![0; 512 * 1024]);
        m.bus.frames = 600;
        m.bus.last_draw_frame = 100;
        for f in [500, 508, 516, 524] {
            m.bus.cdrom.recent.extend([cmd(f, 0x09, &[]), cmd(f, 0x02, &[0x28, 0x51, 0x51]), cmd(f, 0x06, &[])]);
        }
        let w = warnings(&m);
        assert_eq!(w.len(), 1);
        assert_eq!(w[0]["frames_without_draws"], 500);
        assert_eq!(w[0]["screen_blank"], true);
        let l = &w[0]["cd_retry_loop"];
        assert_eq!((l["lba"].as_u64(), l["setlocs"].as_u64(), l["period_frames"].as_u64()), (Some(129726), Some(4), Some(8)));
        assert!(w[0]["message"].as_str().unwrap().contains("retry loop"));
    }

    #[test]
    fn recent_draws_or_short_idle_give_no_warning() {
        let mut m = Machine::new(vec![0; 512 * 1024]);
        m.bus.frames = 600;
        m.bus.last_draw_frame = 590;
        assert!(warnings(&m).is_empty());
    }
}
