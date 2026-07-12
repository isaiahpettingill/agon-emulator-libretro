use agon_ez80_emulator::{gpio, AgonMachine, AgonMachineConfig, GpioVgaFrame, RamInit, SerialLink};
use libretro_backend::{
    libretro_core, AudioVideoInfo, Core, CoreInfo, GameData, LoadGameResult, PixelFormat,
    RuntimeHandle,
};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32};
use std::sync::{mpsc, Arc};

const WIDTH: usize = 640;
const HEIGHT: usize = 480;
const CLOCKS_PER_FRAME: u64 = 18_432_000 / 60;

struct NullSerial;
impl SerialLink for NullSerial {
    fn send(&mut self, _byte: u8) {}
    fn recv(&mut self) -> Option<u8> {
        None
    }
    fn read_clear_to_send(&mut self) -> bool {
        true
    }
}

#[derive(Default)]
struct KeyboardSerial {
    queue: VecDeque<u8>,
}
impl KeyboardSerial {
    fn push_text(&mut self, text: &str) {
        self.queue.extend(text.bytes());
    }
}
#[derive(Clone)]
struct SharedKeyboardSerial(Arc<std::sync::Mutex<KeyboardSerial>>);
impl SerialLink for SharedKeyboardSerial {
    fn send(&mut self, _byte: u8) {}
    fn recv(&mut self) -> Option<u8> {
        self.0.lock().ok()?.queue.pop_front()
    }
    fn read_clear_to_send(&mut self) -> bool {
        true
    }
}

struct RunningMachine {
    machine: AgonMachine,
    cpu: ez80::Cpu,
    gpio_frames: mpsc::Receiver<GpioVgaFrame>,
    _keyboard: Arc<std::sync::Mutex<KeyboardSerial>>,
    _shutdown: Arc<AtomicBool>,
    _soft_reset: Arc<AtomicBool>,
}

pub struct AgonLibretroCore {
    running: Option<RunningMachine>,
    framebuffer: Vec<u16>,
    staged_game: Option<PathBuf>,
    loaded_game: Option<GameData>,
}

impl Default for AgonLibretroCore {
    fn default() -> Self {
        Self {
            running: None,
            framebuffer: vec![0; WIDTH * HEIGHT],
            staged_game: None,
            loaded_game: None,
        }
    }
}

impl AgonLibretroCore {
    fn firmware_path() -> PathBuf {
        std::env::var_os("AGON_LIBRETRO_FIRMWARE")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("firmware/mos_console8.bin"))
    }

    fn sdcard_dir(content_path: Option<&Path>) -> PathBuf {
        if let Some(path) = std::env::var_os("AGON_LIBRETRO_SDCARD") {
            return PathBuf::from(path);
        }
        if let Some(parent) = content_path.and_then(Path::parent) {
            return parent.to_path_buf();
        }
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }

    fn reset_machine(&mut self, content: Option<&Path>) {
        let (tx_gpio, rx_gpio) = mpsc::channel();
        let gpios = Arc::new(gpio::GpioSet::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        let soft_reset = Arc::new(AtomicBool::new(false));
        let keyboard = Arc::new(std::sync::Mutex::new(KeyboardSerial::default()));
        let mut machine = AgonMachine::new(AgonMachineConfig {
            uart0_link: Box::new(SharedKeyboardSerial(keyboard.clone())),
            uart1_link: Box::new(NullSerial),
            soft_reset: soft_reset.clone(),
            emulator_shutdown: shutdown.clone(),
            exit_status: Arc::new(AtomicI32::new(0)),
            paused: Arc::new(AtomicBool::new(false)),
            clockspeed_hz: 18_432_000,
            ram_init: RamInit::Zero,
            mos_bin: Self::firmware_path(),
            gpios,
            tx_gpio_vga_frame: tx_gpio,
            interrupt_precision: 16,
            external_ram_size: 512 * 1024,
        });
        machine.set_sdcard_directory(Self::sdcard_dir(content));
        let mut cpu = ez80::Cpu::new_ez80();
        machine.reset_cpu(&mut cpu);
        if let Some(path) = content.and_then(Path::file_name).and_then(|s| s.to_str()) {
            if let Ok(mut kbd) = keyboard.lock() {
                let lower = path.to_ascii_lowercase();
                if lower.ends_with(".bas") || lower.ends_with(".bbc") {
                    kbd.push_text(&format!("LOAD \"{}\"\rRUN\r", path));
                } else if lower.ends_with(".bin") {
                    kbd.push_text(&format!("LOAD \"{}\"\rCALL &40000\r", path));
                }
            }
        }
        self.running = Some(RunningMachine {
            machine,
            cpu,
            gpio_frames: rx_gpio,
            _keyboard: keyboard,
            _shutdown: shutdown,
            _soft_reset: soft_reset,
        });
    }

    fn draw_placeholder(&mut self) {
        for y in 0..HEIGHT {
            for x in 0..WIDTH {
                self.framebuffer[y * WIDTH + x] = if ((x / 16) + (y / 16)) % 2 == 0 {
                    0x39e7
                } else {
                    0x0000
                };
            }
        }
    }
}

impl Core for AgonLibretroCore {
    fn info() -> CoreInfo {
        CoreInfo::new("Fab Agon", env!("CARGO_PKG_VERSION"))
            .supports_roms_with_extension("bin")
            .supports_roms_with_extension("bas")
            .supports_roms_with_extension("bbc")
            .requires_path_when_loading_roms()
    }

    fn on_load_game(&mut self, game_data: GameData) -> LoadGameResult {
        self.staged_game = game_data.path().map(PathBuf::from);
        let content = self.staged_game.clone();
        self.reset_machine(content.as_deref());
        self.loaded_game = Some(game_data);
        LoadGameResult::Success(
            AudioVideoInfo::new()
                .video(WIDTH as u32, HEIGHT as u32, 60.0, PixelFormat::RGB565)
                .audio(48_000.0),
        )
    }

    fn on_unload_game(&mut self) -> GameData {
        self.running = None;
        self.loaded_game
            .take()
            .expect("libretro unloaded a game that was never loaded")
    }

    fn on_run(&mut self, handle: &mut RuntimeHandle) {
        if let Some(running) = &mut self.running {
            running
                .machine
                .run_for_cycles(&mut running.cpu, CLOCKS_PER_FRAME);
            running.machine.gpios().b.set_input_pin(1, true);
            running.machine.gpios().b.set_input_pin(1, false);
            while let Ok(frame) = running.gpio_frames.try_recv() {
                let width = frame.width.min(WIDTH as u32) as usize;
                let height = frame.height.min(HEIGHT as u32) as usize;
                for y in 0..height {
                    for x in 0..width {
                        let src = y * frame.line_length_cycles as usize
                            + x
                            + frame.scanline_img_start as usize;
                        if let Some(&px) = frame.picture.get(src) {
                            let r = ((px & 0b1110_0000) as u16 >> 5) * 31 / 7;
                            let g = ((px & 0b0001_1100) as u16 >> 2) * 63 / 7;
                            let b = (px & 0b0000_0011) as u16 * 31 / 3;
                            self.framebuffer[y * WIDTH + x] = (r << 11) | (g << 5) | b;
                        }
                    }
                }
            }
        } else {
            self.draw_placeholder();
        }
        let audio = vec![0i16; 1600];
        handle.upload_audio_frame(&audio);
        let bytes = unsafe {
            std::slice::from_raw_parts(
                self.framebuffer.as_ptr() as *const u8,
                self.framebuffer.len() * 2,
            )
        };
        handle.upload_video_frame(bytes);
    }

    fn on_reset(&mut self) {
        let content = self.staged_game.clone();
        self.reset_machine(content.as_deref());
    }
}

libretro_core!(AgonLibretroCore);
