// Headless Mednafen frontend for psx-oracle (src/ffi.rs): the callbacks
// Mednafen's core expects from its frontend, and a small C API.
// GPL-2.0-or-later, like the Mednafen sources it is built with.
#include <mednafen/mednafen.h>
#include <mednafen/mednafen-driver.h>
#include <mednafen/state-driver.h>
#include <mednafen/movie-driver.h>
#include <mednafen/netplay-driver.h>
#include <mednafen/video.h>
#include <mednafen/NativeVFS.h>
#include <mednafen/psx/psx.h>

#include <cstdio>
#include <cstring>
#include <memory>
#include <string>
#include <vector>

using namespace Mednafen;

namespace MDFN_IEN_PSX
{
extern uint64 psxm_vblanks;  // psx/gpu.cpp
}

namespace
{
std::string last_error;
bool verbose = false;
MDFNGI* game = nullptr;
std::unique_ptr<MDFN_Surface> surface;
std::vector<int32> line_widths;
uint8* pad = nullptr;
bool dualshock = false;
EmulateSpecStruct last;
void (*hook)(uint32_t pc, int breakpoint, void* user) = nullptr;
void* hook_user = nullptr;
}  // namespace

namespace Mednafen
{
void MDFND_OutputNotice(MDFN_NoticeType t, const char* s) noexcept
{
 if(t == MDFN_NOTICE_ERROR)
  last_error = s;
 if(verbose || t == MDFN_NOTICE_ERROR)
  fprintf(stderr, "mednafen: %s\n", s);
}

void MDFND_OutputInfo(const char* s) noexcept
{
 if(verbose)
  fputs(s, stderr);
}

void MDFND_MidSync(EmulateSpecStruct*, const unsigned) {}
bool MDFND_CheckNeedExit(void) { return false; }
void MDFND_MediaSetNotification(uint32, uint32, uint32, uint32) {}
void MDFND_NetplayText(const char*, bool) {}
void MDFND_NetplaySetHints(bool, bool, uint32) {}
void MDFND_SetStateStatus(StateStatusStruct* status) noexcept { delete status; }
void MDFND_SetMovieStatus(StateStatusStruct* status) noexcept { delete status; }
}  // namespace Mednafen

extern "C" {

struct mdh_frame
{
 int32_t x, y, w, h;
 // Width of the first displayed line (the PSX core sets every line's width).
 int32_t line_width;
 int32_t interlaced, field;
 // CPU cycles (Mednafen's master clock) since the previous frame.
 int64_t master_cycles;
};

struct mdh_regs
{
 uint32_t gpr[32];
 uint32_t hi, lo, pc, sr, cause, epc;
 // The delayed load in flight: register (32 = none) and value.
 uint32_t load_reg, load_value;
};

const char* mdh_error(void) { return last_error.c_str(); }

const char* mdh_version(void) { return MEDNAFEN_VERSION; }

void mdh_verbose(int on) { verbose = on != 0; }

static bool set(const char* name, const std::string& value)
{
 if(MDFNI_SetSetting(name, value))
  return true;
 last_error = std::string("can't set ") + name + " to " + value;
 return false;
}

// `base` holds Mednafen's own files (settings, memory cards); `bios` is used
// for every region and `region` ("jp", "na" or "eu") is the console's.
// `media` is a disc image or PS-X EXE, or NULL to boot the BIOS with the
// drive closed and empty. Port 1 is a digital pad, or a DualShock.
int mdh_open(const char* base, const char* bios, const char* media, const char* region, int with_dualshock)
{
 last_error.clear();
 if(!MDFNI_Init() || !MDFNI_InitFinalize(base))
  return -1;
 for(const char* setting : { "psx.bios_jp", "psx.bios_na", "psx.bios_eu" })
  if(!set(setting, bios))
   return -1;
 // Any BIOS dump; the console's region is the BIOS's, not the disc's.
 if(!set("psx.bios_sanity", "0") || !set("psx.region_autodetect", "0") || !set("psx.region_default", region))
  return -1;
 if(!set("filesys.path_sav", base) || !set("filesys.path_state", base) || !set("filesys.path_cheat", base))
  return -1;
 // No memory cards, as in the native core by default.
 if(!set("psx.input.port1.memcard", "0") || !set("psx.input.port2.memcard", "0"))
  return -1;
 // Read the whole disc into memory first, so CD timing never waits on the host.
 if(!set("cd.image_memcache", "1"))
  return -1;
 game = media ? MDFNI_LoadGame(nullptr, &NVFS, media) : MDFNI_LoadNoDisc("psx");
 if(!game)
 {
  if(last_error.empty())
   last_error = media ? std::string("can't load ") + media : std::string("can't boot the BIOS");
  return -1;
 }
 // The core powers on with the tray open; frontends put the media in, here
 // before the first frame, as if it had been in the drive at power-on.
 for(uint32 d = 0; d < game->RMD->DrivesDefaults.size(); d++)
 {
  auto const& dd = game->RMD->DrivesDefaults[d];
  if(!MDFNI_SetMedia(d, dd.State, dd.Media, dd.Orientation))
   return -1;
 }
 surface.reset(new MDFN_Surface(nullptr, game->fb_width, game->fb_height, game->fb_width, MDFN_PixelFormat::ABGR32_8888));
 line_widths.assign(game->fb_height, 0);
 // Port 1: device 1 (gamepad) or 2 (dualshock) in frontio.cpp's list; port 2: nothing.
 dualshock = with_dualshock != 0;
 pad = MDFNI_SetInput(0, dualshock ? 2 : 1);
 MDFNI_SetInput(1, 0);
 return 0;
}

// Buttons held from the next frame: active high, in the pad's own bit order
// (bit 0 SELECT, 1 L3, 2 R3, 3 START, 4 UP, 5 RIGHT, 6 DOWN, 7 LEFT, 8 L2,
// 9 R2, 10 L1, 11 R1, 12 TRIANGLE, 13 CIRCLE, 14 CROSS, 15 SQUARE). Sticks
// (DualShock only) are lx, ly, rx, ry, 0-255 with 128 centred.
void mdh_set_pad(uint16_t buttons, const uint8_t* sticks)
{
 if(!pad)
  return;
 pad[0] = buttons & 0xFF;
 pad[1] = buttons >> 8;
 if(dualshock)
 {
  pad[2] = 0;  // the Analog button
  // Right stick first (x, y), then the left; 16-bit, 0-65535.
  const uint8_t order[4] = { sticks[2], sticks[3], sticks[0], sticks[1] };
  for(int i = 0; i < 4; i++)
   MDFN_en16lsb(&pad[3 + i * 2], order[i] * 257);
 }
}

uint64_t mdh_vblanks(void) { return MDFN_IEN_PSX::psxm_vblanks; }

// Runs to the next vblank (Mednafen's frame may end earlier, so it loops).
// Without `render` the picture isn't drawn into the frame buffer (VRAM is
// unaffected).
int mdh_frame(struct mdh_frame* out, int render)
{
 const uint64 target = MDFN_IEN_PSX::psxm_vblanks + 1;
 int64_t cycles = 0;
 while(MDFN_IEN_PSX::psxm_vblanks < target)
 {
  EmulateSpecStruct espec;
  espec.surface = surface.get();
  espec.LineWidths = line_widths.data();
  espec.SoundRate = 0;
  espec.SoundBuf = nullptr;
  espec.SoundBufMaxSize = 0;
  espec.skip = !render;
  MDFNI_Emulate(&espec);
  cycles += espec.MasterCycles;
  last = espec;
 }
 out->x = last.DisplayRect.x;
 out->y = last.DisplayRect.y;
 out->w = last.DisplayRect.w;
 out->h = last.DisplayRect.h;
 out->line_width = line_widths[last.DisplayRect.y];
 out->interlaced = last.InterlaceOn;
 out->field = last.InterlaceField;
 out->master_cycles = cycles;
 return 0;
}

// The frame buffer as R, G, B, A bytes; `pitch` is in pixels.
const uint32_t* mdh_pixels(int32_t* pitch)
{
 *pitch = surface->pitchinpix;
 return surface->pixels;
}

// One of the PSX debugger's address spaces: "cpu", "ram", "spu" or "gpu".
int mdh_read(const char* space, uint32_t address, uint32_t length, uint8_t* buffer)
{
 for(auto const& a : *game->Debugger->AddressSpaces)
  if(a.name == space)
  {
   a.GetAddressSpaceBytes(space, address, length, buffer);
   return 0;
  }
 last_error = std::string("no address space ") + space;
 return -1;
}

// A side-effect-free read of the CPU's (logical) address space.
uint32_t mdh_peek(uint32_t address, uint32_t bytes) { return game->Debugger->MemPeek(address, bytes, true, true); }

void mdh_regs(struct mdh_regs* r)
{
 using MDFN_IEN_PSX::CPU;
 using MDFN_IEN_PSX::PS_CPU;
 for(int i = 0; i < 32; i++)
  r->gpr[i] = CPU->GetRegister(PS_CPU::GSREG_GPR + i, nullptr, 0);
 r->hi = CPU->GetRegister(PS_CPU::GSREG_HI, nullptr, 0);
 r->lo = CPU->GetRegister(PS_CPU::GSREG_LO, nullptr, 0);
 r->pc = CPU->GetRegister(PS_CPU::GSREG_PC, nullptr, 0);
 r->sr = CPU->GetRegister(PS_CPU::GSREG_SR, nullptr, 0);
 r->cause = CPU->GetRegister(PS_CPU::GSREG_CAUSE, nullptr, 0);
 r->epc = CPU->GetRegister(PS_CPU::GSREG_EPC, nullptr, 0);
 CPU->GetPendingLoad(&r->load_reg, &r->load_value);
}

static void step_hook(const MDFN_IEN_PSX::pscpu_timestamp_t, uint32 pc)
{
 // While DMA holds the CPU, the hook runs every cycle without the instruction executing.
 if(hook && !MDFN_IEN_PSX::CPU->IsHalted())
  hook(pc, 0, hook_user);
}

static void breakpoint_hook(uint32 pc, bool bpoint)
{
 if(hook)
  hook(pc, bpoint, hook_user);
}

// Calls `cb` before every instruction (`mdh_regs` is valid inside it), or
// with NULL stops. The CPU then runs Mednafen's debug interpreter, whose
// timing is the same as its normal one.
void mdh_step_hook(void (*cb)(uint32_t pc, int breakpoint, void* user), void* user)
{
 hook = cb;
 hook_user = user;
 MDFN_IEN_PSX::CPU->SetCPUHook(cb ? step_hook : nullptr, nullptr);
}

// Calls `cb` before CPU stores that may touch [address, address + length)
// (physical), with `breakpoint` set; the caller checks the store's exact
// range. Mednafen matches the store's logical start address, so each
// segment's mirror is watched, from 3 bytes early. DMA writes aren't seen.
void mdh_write_watch(uint32_t address, uint32_t length, void (*cb)(uint32_t pc, int breakpoint, void* user), void* user)
{
 hook = cb;
 hook_user = user;
 for(uint32 segment : { 0x00000000U, 0x80000000U, 0xA0000000U })
  game->Debugger->AddBreakPoint(BPOINT_WRITE, segment + address - 3, segment + address + length - 1, true);
 game->Debugger->SetCPUCallback(breakpoint_hook, false);
}

int mdh_save_state(const char* path)
{
 return MDFNI_SaveState(path, nullptr, surface.get(), &last.DisplayRect, line_widths.data()) ? 0 : -1;
}

int mdh_load_state(const char* path) { return MDFNI_LoadState(path, nullptr) ? 0 : -1; }

void mdh_close(void)
{
 if(game)
  MDFNI_CloseGame();
 game = nullptr;
 MDFNI_Kill();
}
}
