// Backend probe: known RAM writes, simple drawing and pad input.
//
// Observable contract (addresses come from basic.map / the ELF symbols):
//   g_probe_magic   = 0x50535831 ("PSX1") written once at startup
//   g_probe_frames  incremented once per displayed frame by PadScene::frame
//   g_probe_cross   incremented on frames where pad 1 Cross is held
//   g_probe_last_x  x position of the moving rectangle, written every frame
// Screen: dark blue background, a red 32x32 rectangle moving right
// (x = 16 + frames % 256, y = 100) and a fixed gouraud triangle.
#include "psyqo/application.hh"
#include "psyqo/gpu.hh"
#include "psyqo/primitives/rectangles.hh"
#include "psyqo/primitives/triangles.hh"
#include "psyqo/scene.hh"
#include "psyqo/simplepad.hh"

extern "C" {
volatile uint32_t g_probe_magic = 0;
volatile uint32_t g_probe_frames = 0;
volatile uint32_t g_probe_cross = 0;
volatile uint32_t g_probe_last_x = 0;
}

namespace {

class Probe final : public psyqo::Application {
    void prepare() override;
    void createScene() override;

  public:
    psyqo::SimplePad m_pad;
};

class ProbeScene final : public psyqo::Scene {
    void frame() override;
};

Probe probe;
ProbeScene scene;

}  // namespace

void Probe::prepare() {
    psyqo::GPU::Configuration config;
    config.set(psyqo::GPU::Resolution::W320)
        .set(psyqo::GPU::VideoMode::AUTO)
        .set(psyqo::GPU::ColorMode::C15BITS)
        .set(psyqo::GPU::Interlace::PROGRESSIVE);
    gpu().initialize(config);
}

void Probe::createScene() {
    g_probe_magic = 0x50535831;
    m_pad.initialize();
    pushScene(&scene);
}

void ProbeScene::frame() {
    auto& gpu = probe.gpu();
    gpu.clear({{.r = 0, .g = 0, .b = 64}});

    uint32_t frames = g_probe_frames + 1;
    g_probe_frames = frames;
    if (probe.m_pad.isButtonPressed(psyqo::SimplePad::Pad1, psyqo::SimplePad::Cross)) {
        g_probe_cross = g_probe_cross + 1;
    }

    int16_t x = int16_t(16 + (frames & 0xff));
    g_probe_last_x = uint32_t(x);
    psyqo::Prim::Rectangle rect(psyqo::Color{{.r = 255, .g = 0, .b = 0}});
    rect.position = {{.x = x, .y = 100}};
    rect.size = {{.x = 32, .y = 32}};
    gpu.sendPrimitive(rect);

    psyqo::Prim::GouraudTriangle tri(psyqo::Color{{.r = 255, .g = 0, .b = 0}});
    tri.setPointA({{.x = 160, .y = 20}})
        .setPointB({{.x = 120, .y = 80}})
        .setPointC({{.x = 200, .y = 80}})
        .setColorB({{.r = 0, .g = 255, .b = 0}})
        .setColorC({{.r = 0, .g = 0, .b = 255}});
    gpu.sendPrimitive(tri);
}

int main() { return probe.run(); }
