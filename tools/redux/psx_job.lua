-- psx batch job for PCSX-Redux. GPL-2.0-or-later (runs inside PCSX-Redux).
--
-- Launched by the psx supervisor as:
--   pcsx-redux ... -exec "JOB_DIR='<dir>'" -dofile psx_job.lua
-- Reads <dir>/params.lua (a table) and writes results into <dir>:
--   result.json, ram.bin (2 MiB), scratch.bin (1 KiB), vram.bin (1 MiB),
--   screen.bin (+ dims in result.json), state.bin (if save_state),
--   writes.jsonl (watch hits), log.txt
--
-- Frames are counted as GPU vsync events (fields) after the job starts.

local ffi = require('ffi')
local P = dofile(JOB_DIR .. '/params.lua')

-- Pacing (SPU NullSync/Speed) is set by the supervisor in config/pcsx.json,
-- because NullSync is only read when audio starts.

local function path(name) return JOB_DIR .. '/' .. name end

local function write_file(name, ptr, size)
    local f = Support.File.open(path(name), 'TRUNCATE')
    f:write(ptr, size)
    f:close()
end

local function write_slice(name, slice)
    local f = Support.File.open(path(name), 'TRUNCATE')
    f:writeMoveSlice(slice)
    f:close()
end

local function json_string(s)
    return '"' .. tostring(s):gsub('[%c"\\]', function(c) return string.format('\\u%04x', c:byte()) end) .. '"'
end

-- Minimal JSON encoder for numbers, strings, booleans, arrays and objects.
local function json(v)
    local t = type(v)
    if t == 'number' then
        if v ~= v or v == math.huge or v == -math.huge then return 'null' end
        if v == math.floor(v) then return string.format('%d', v) end
        return string.format('%.17g', v)
    elseif t == 'string' then
        return json_string(v)
    elseif t == 'boolean' then
        return v and 'true' or 'false'
    elseif t == 'table' then
        if #v > 0 or next(v) == nil and v.__array then
            local parts = {}
            for i = 1, #v do parts[i] = json(v[i]) end
            return '[' .. table.concat(parts, ',') .. ']'
        end
        local parts = {}
        for k, val in pairs(v) do
            if k ~= '__array' then parts[#parts + 1] = json_string(k) .. ':' .. json(val) end
        end
        return '{' .. table.concat(parts, ',') .. '}'
    end
    return 'null'
end

local regs = PCSX.getRegisters()
local GPR_NAMES = { 'r0', 'at', 'v0', 'v1', 'a0', 'a1', 'a2', 'a3', 't0', 't1', 't2', 't3', 't4', 't5', 't6', 't7',
    's0', 's1', 's2', 's3', 's4', 's5', 's6', 's7', 't8', 't9', 'k0', 'k1', 'gp', 'sp', 'fp', 'ra' }

local function snapshot_regs()
    local r = { pc = tonumber(regs.pc), lo = tonumber(regs.GPR.n.lo), hi = tonumber(regs.GPR.n.hi) }
    for i = 0, 31 do r[GPR_NAMES[i + 1]] = tonumber(regs.GPR.r[i]) end
    r.cop0_sr = tonumber(regs.CP0.r[12])
    r.cop0_cause = tonumber(regs.CP0.r[13])
    r.cop0_epc = tonumber(regs.CP0.r[14])
    return r
end

-- Controller replay: P.replay = { { frame = n, buttons = { 'CROSS', ... } }, ... }
-- Each event replaces the held set from that frame on (frames relative to job start).
local pad = PCSX.SIO0.slots[1].pads[1]
local BUTTON = PCSX.CONSTS.PAD.BUTTON
local held = {}
local function apply_buttons(names)
    for name, _ in pairs(held) do pad.clearOverride(BUTTON[name]) end
    held = {}
    for _, name in ipairs(names) do
        local b = BUTTON[name]
        if b == nil then error('unknown button ' .. name) end
        pad.setOverride(b)
        held[name] = true
    end
end

-- Write watches: P.watches = { { addr = 0x80014808, width = 4 }, ... }
local mem = PCSX.getMemPtr()
local hits = {}
local hit_count = 0
local max_hits = P.max_watch_hits or 10000
local frame = 0
local function read_u32(addr)
    local off = bit.band(addr, 0x1fffff)
    return tonumber(ffi.cast('uint32_t*', mem + bit.band(off, bit.bnot(3)))[0])
end
local watch_bps = {}
for _, w in ipairs(P.watches or {}) do
    local bp = PCSX.addBreakpoint(w.addr, 'Write', w.width or 4, 'psx-watch', function(address, width, cause)
        hit_count = hit_count + 1
        if #hits < max_hits then
            local r = snapshot_regs()
            hits[#hits + 1] = { frame = frame, address = tonumber(address), width = tonumber(width),
                cycle = tonumber(PCSX.getCPUCycles()), pc = r.pc, ra = r.ra, sp = r.sp,
                old_word = read_u32(tonumber(address)) }
        end
        return true
    end)
    watch_bps[#watch_bps + 1] = bp
end

local function finish(reason)
    print('psx-job: finishing')
    for _, bp in ipairs(watch_bps) do bp:remove() end
    write_file('ram.bin', PCSX.getMemPtr(), 2 * 1024 * 1024)
    write_file('scratch.bin', PCSX.getScratchPtr(), 1024)
    write_slice('vram.bin', PCSX.GPU.getVRAM())
    local shot = PCSX.GPU.takeScreenShot()
    write_slice('screen.bin', shot.data)
    if P.save_state then write_slice('state.bin', PCSX.createSaveState()) end
    local f = Support.File.open(path('writes.jsonl'), 'TRUNCATE')
    for _, h in ipairs(hits) do f:write(json(h) .. '\n') end
    f:close()
    local result = {
        termination = reason,
        requested_frames = P.frames,
        reached_frames = frame,
        cpu_cycles = tonumber(PCSX.getCPUCycles()),
        registers = snapshot_regs(),
        screen = { width = shot.width, height = shot.height, bpp = (shot.bpp == 0) and 16 or 24 },
        watch_hits = hit_count,
        watch_hits_recorded = #hits,
        saved_state = P.save_state and true or false,
    }
    f = Support.File.open(path('result.json'), 'TRUNCATE')
    f:write(json(result))
    f:close()
    PCSX.quit(0)
end

local replay_index = 1
local replay = P.replay or {}
local function step_replay()
    while replay[replay_index] and replay[replay_index].frame <= frame do
        apply_buttons(replay[replay_index].buttons)
        replay_index = replay_index + 1
    end
end

-- With a state to load, the load is requested on the first vsync (once the
-- machine is running) and frame counting starts when Redux reports it loaded.
local counting = not P.load_state
local load_requested = false

JOB_STATE_LISTENER = PCSX.Events.createEventListener('ExecutionFlow::SaveStateLoaded', function()
    if load_requested and not counting then
        print('psx-job: state loaded')
        counting = true
        frame = 0
        step_replay()
    end
end)

-- Keep the listeners referenced for the life of the job.
JOB_VSYNC_LISTENER = PCSX.Events.createEventListener('GPU::Vsync', function()
    if not counting then
        if not load_requested then
            load_requested = true
            local f = Support.File.open(P.load_state)
            PCSX.loadSaveState(f)
            f:close()
        end
        return
    end
    frame = frame + 1
    step_replay()
    if frame >= P.frames then
        PCSX.pauseEmulator()
        PCSX.nextTick(function() finish('frames_reached') end)
    end
end)

print('psx-job: starting, frames=' .. P.frames)
if counting then step_replay() end
PCSX.resumeEmulator()
