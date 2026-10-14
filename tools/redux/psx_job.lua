-- psx batch job for PCSX-Redux. GPL-2.0-or-later (runs inside PCSX-Redux).
--
-- Launched by the psx supervisor as:
--   pcsx-redux ... -exec "JOB_DIR='<dir>'" -dofile psx_job.lua
-- Reads <dir>/params.lua (a table) and writes results into <dir>:
--   result.json, ram.bin (2 MiB), scratch.bin (1 KiB), vram.bin (1 MiB),
--   screen.bin (+ dims in result.json), state.bin (if save_state),
--   writes.jsonl (watch hits), cd.jsonl / bios.jsonl (when traced)
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

-- Memory helpers (RAM and BIOS ROM only; I/O is not readable here).
local mem = PCSX.getMemPtr()
local rom = PCSX.getRomPtr()
local frame = 0

local function read_u32(addr)
    local phys = bit.band(addr, 0x1fffffff)
    if phys < 0x800000 then
        return tonumber(ffi.cast('uint32_t*', mem + bit.band(phys, 0x1ffffc))[0])
    elseif phys >= 0x1fc00000 and phys < 0x1fc80000 then
        return tonumber(ffi.cast('uint32_t*', rom + bit.band(phys - 0x1fc00000, bit.bnot(3)))[0])
    end
    return nil
end

local function read_cstring(addr, max)
    local out = {}
    for i = 0, (max or 128) - 1 do
        local w = read_u32(addr + i - (addr + i) % 4)
        if w == nil then break end
        local c = bit.band(bit.rshift(w, ((addr + i) % 4) * 8), 0xff)
        if c == 0 then break end
        out[#out + 1] = string.char(c)
    end
    return table.concat(out)
end

-- New value of a CPU store, decoded from the store instruction at pc (or the
-- next one, for stores in a branch delay slot). nil when it cannot be decoded.
local STORES = { [0x28] = 1, [0x29] = 2, [0x2b] = 4 }
local function store_value(pc, width)
    for _, at in ipairs({ pc, pc + 4 }) do
        local ins = read_u32(at)
        if ins then
            local op = bit.rshift(ins, 26)
            if STORES[op] then
                local rt = bit.band(bit.rshift(ins, 16), 31)
                local v = tonumber(regs.GPR.r[rt])
                local w = STORES[op]
                if w < 4 then v = bit.band(v, bit.lshift(1, w * 8) - 1) end
                if v < 0 then v = v + 4294967296 end
                return v, at
            end
        end
    end
    return nil, nil
end

local function writer_of(cause)
    local ch = cause:match('DMA channel (%d+) write')
    if ch then return 'dma', tonumber(ch) end
    return 'cpu', nil
end

-- Write watches: P.watches = { { addr = 0x80014808, width = 4 }, ... }
local hits = {}
local hit_count = 0
local max_hits = P.max_watch_hits or 10000
local bps = {}
for _, w in ipairs(P.watches or {}) do
    bps[#bps + 1] = PCSX.addBreakpoint(w.addr, 'Write', w.width or 4, 'psx-watch', function(address, width, cause)
        hit_count = hit_count + 1
        if #hits < max_hits then
            local kind, channel = writer_of(cause)
            local r = snapshot_regs()
            local h = { frame = frame, address = tonumber(address), width = tonumber(width), writer = kind,
                cycle = tonumber(PCSX.getCPUCycles()), pc = r.pc, ra = r.ra, sp = r.sp,
                old_word = read_u32(tonumber(address)) }
            if kind == 'dma' then
                -- The CPU PC at DMA completion is not the writer; keep only the channel.
                h.dma_channel = channel
                h.pc, h.ra, h.sp = nil, nil, nil
            else
                h.value, h.store_pc = store_value(r.pc, tonumber(width))
            end
            hits[#hits + 1] = h
        end
        return true
    end)
end

-- Event logs written as JSON lines.
local logs = {}
local function log_event(name, ev)
    local l = logs[name]
    if not l then
        l = { n = 0, items = {} }
        logs[name] = l
    end
    l.n = l.n + 1
    if l.n <= (P.max_events or 200000) then l.items[#l.items + 1] = ev end
end

-- CD DMA content capture. When CHCR starts a transfer, a one-shot write
-- breakpoint is placed over the destination; Redux fires it (cause "DMA
-- channel 3 write") right after copying, before any CPU code can touch the
-- bytes. Spent breakpoints are disabled in their callback and removed later,
-- because removing a breakpoint inside its own callback is unsafe.
local pending_dma = nil
local spent_bps = {}
local function sweep_spent()
    for i = #spent_bps, 1, -1 do
        spent_bps[i]:remove()
        spent_bps[i] = nil
    end
end
local function ram_bytes(addr, n)
    local phys = bit.band(addr, 0x1fffff)
    n = math.min(n, 0x200000 - phys)
    return ffi.string(mem + phys, n)
end
local function adler32(str)
    local a, b = 1, 0
    for i = 1, #str do
        a = (a + str:byte(i)) % 65521
        b = (b + a) % 65521
    end
    return b * 65536 + a
end
local function capture_dma(p, timing)
    local data = ram_bytes(p.madr, p.bytes)
    local head = {}
    for i = 1, math.min(64, #data) do head[i] = string.format('%02x', data:byte(i)) end
    log_event('cd', { frame = p.frame, cycle = p.cycle, reg = 'dma3_data', madr = p.madr, bytes = p.bytes,
        head = table.concat(head), adler32 = adler32(data), timing = timing })
end

-- Fallback for a transfer whose one-shot breakpoint never fired (e.g. outside RAM).
local function flush_pending()
    local p = pending_dma
    if p and not p.captured then capture_dma(p, 'late') end
    pending_dma = nil
end

-- CD-ROM controller and DMA channel 3 register writes (P.trace_cd).
if P.trace_cd then
    -- The index register selects what regs 1-3 mean; it is tracked, not logged
    -- (games rewrite it on every status poll).
    local cd_index = 0
    bps[#bps + 1] = PCSX.addBreakpoint(0x1f801800, 'Write', 1, 'psx-cd', function()
        flush_pending()
        local v = store_value(regs.pc, 1)
        if v then cd_index = bit.band(v, 3) end
        return true
    end)
    local CD_REGS = { [0x1f801801] = 1, [0x1f801802] = 2, [0x1f801803] = 3 }
    for addr, n in pairs(CD_REGS) do
        bps[#bps + 1] = PCSX.addBreakpoint(addr, 'Write', 1, 'psx-cd', function()
            flush_pending()
            local r = snapshot_regs()
            log_event('cd', { frame = frame, cycle = tonumber(PCSX.getCPUCycles()), reg = n, index = cd_index,
                value = store_value(r.pc, 1), pc = r.pc })
            return true
        end)
    end
    local DMA3 = { [0x1f8010b0] = 'madr', [0x1f8010b4] = 'bcr', [0x1f8010b8] = 'chcr' }
    local dma3 = {}
    for addr, name in pairs(DMA3) do
        bps[#bps + 1] = PCSX.addBreakpoint(addr, 'Write', 4, 'psx-dma3', function(address, width, cause)
            flush_pending()
            local r = snapshot_regs()
            local v = store_value(r.pc, 4)
            local cycle = tonumber(PCSX.getCPUCycles())
            log_event('cd', { frame = frame, cycle = cycle, reg = 'dma3_' .. name, value = v, pc = r.pc })
            if v then dma3[name] = v end
            if name == 'chcr' and v and bit.band(v, 0x01000000) ~= 0 and dma3.madr and dma3.bcr then
                local bs, bc = bit.band(dma3.bcr, 0xffff), bit.rshift(dma3.bcr, 16)
                if bs == 0 then bs = 1 end
                if bc == 0 then bc = 1 end
                local p = { frame = frame, cycle = cycle, madr = dma3.madr, bytes = bs * bc * 4 }
                pending_dma = p
                local phys = bit.band(p.madr, 0x1fffff)
                if bit.band(p.madr, 0x1fffffff) < 0x800000 then
                    local once
                    once = PCSX.addBreakpoint(0x80000000 + phys, 'Write', p.bytes, 'psx-dma3-data', function(a, w, cause)
                        if not p.captured and cause:match('DMA channel 3 write') then
                            p.captured = true
                            -- Redux clamps to the bytes left in its data FIFO.
                            p.bytes = math.min(p.bytes, tonumber(w))
                            capture_dma(p, 'at_dma')
                            once:disable()
                            spent_bps[#spent_bps + 1] = once
                        end
                        return true
                    end)
                end
            end
            return true
        end)
    end
end

-- BIOS function calls through the A0/B0/C0 vectors (P.trace_bios).
-- Every call is counted; these frequent ones are not logged individually:
-- B(0Bh) TestEvent, B(17h) ReturnFromException, B(3Dh)/A(3Ch) putchar.
local bios_counts = {}
local BIOS_QUIET = { B0_11 = true, B0_23 = true, B0_61 = true, A0_60 = true }
if P.trace_bios then
    for _, vec in ipairs({ 0xa0, 0xb0, 0xc0 }) do
        bps[#bps + 1] = PCSX.addBreakpoint(vec, 'Exec', 4, 'psx-bios', function()
            local r = snapshot_regs()
            local fn = bit.band(r.t1, 0xff)
            local key = string.format('%X_%d', vec, fn)
            bios_counts[key] = (bios_counts[key] or 0) + 1
            if BIOS_QUIET[key] then return true end
            local ev = { frame = frame, cycle = tonumber(PCSX.getCPUCycles()), table = string.format('%X', vec),
                fn = fn, a0 = r.a0, a1 = r.a1, a2 = r.a2, a3 = r.a3, ra = r.ra }
            -- String arguments of file/exec calls: A(51h) LoadExec, B(32h) open, A(00h) open, B(42h) firstfile.
            if (vec == 0xa0 and (fn == 0x51 or fn == 0x00 or fn == 0x42)) or (vec == 0xb0 and (fn == 0x32 or fn == 0x42)) then
                ev.path = read_cstring(r.a0, 96)
            end
            log_event('bios', ev)
            return true
        end)
    end
end

local function finish(reason)
    print('psx-job: finishing')
    flush_pending()
    sweep_spent()
    for _, bp in ipairs(bps) do bp:remove() end
    local counts = {}
    for name, l in pairs(logs) do
        local lf = Support.File.open(path(name .. '.jsonl'), 'TRUNCATE')
        for _, ev in ipairs(l.items) do lf:write(json(ev) .. '\n') end
        lf:close()
        counts[name] = { events = l.n, recorded = #l.items }
    end
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
        event_logs = counts,
        bios_call_counts = bios_counts,
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
    flush_pending()
    sweep_spent()
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
