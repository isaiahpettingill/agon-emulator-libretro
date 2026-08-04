use agon_ez80_emulator::{gpio, AgonMachine, AgonMachineConfig, RamInit, SerialLink};
use libloading::Library;
use libretro_backend::{
    libretro_core, AudioVideoInfo, Core, CoreInfo, GameData, LoadGameResult, PixelFormat,
    RuntimeHandle,
};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI32};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;

const WIDTH: usize = 640;
const HEIGHT: usize = 480;
const CLOCKS_PER_FRAME: u64 = 18_432_000 / 60;
const AUDIO_FRAMES_PER_VIDEO_FRAME: usize = 48_000 / 60;
const BUNDLED_MOS: &[u8] = include_bytes!("../../firmware/mos_console8.bin");
const BUNDLED_MOS_MAP: &[u8] = include_bytes!("../../firmware/mos_console8.map");
const BUNDLED_VDP: &[u8] = include_bytes!(env!("AGON_BUNDLED_VDP"));

#[cfg(target_os = "windows")]
const VDP_FILENAME: &str = "vdp_console8.dll";
#[cfg(target_os = "macos")]
const VDP_FILENAME: &str = "vdp_console8.dylib";
#[cfg(not(any(target_os = "windows", target_os = "macos")))]
const VDP_FILENAME: &str = "vdp_console8.so";

type VoidFn = unsafe extern "C" fn();
type CopyFramebufferFn = unsafe extern "C" fn(*mut u32, *mut u32, *mut u8, *mut f32);
type SendByteFn = unsafe extern "C" fn(u8);
type ReceiveByteFn = unsafe extern "C" fn(*mut u8) -> bool;
type CtsFn = unsafe extern "C" fn() -> bool;
type AudioFn = unsafe extern "C" fn(*mut u8, u32);
type SendPs2KeyFn = unsafe extern "C" fn(u16, u8);

const KEYBOARD_MAP: &[(u32, u16)] = &[
    (8, 0x66),
    (9, 0x0d),
    (13, 0x5a),
    (27, 0x76),
    (32, 0x29),
    (39, 0x52),
    (44, 0x41),
    (45, 0x4e),
    (46, 0x49),
    (47, 0x4a),
    (48, 0x45),
    (49, 0x16),
    (50, 0x1e),
    (51, 0x26),
    (52, 0x25),
    (53, 0x2e),
    (54, 0x36),
    (55, 0x3d),
    (56, 0x3e),
    (57, 0x46),
    (59, 0x4c),
    (61, 0x55),
    (91, 0x54),
    (92, 0x5d),
    (93, 0x5b),
    (96, 0x0e),
    (97, 0x1c),
    (98, 0x32),
    (99, 0x21),
    (100, 0x23),
    (101, 0x24),
    (102, 0x2b),
    (103, 0x34),
    (104, 0x33),
    (105, 0x43),
    (106, 0x3b),
    (107, 0x42),
    (108, 0x4b),
    (109, 0x3a),
    (110, 0x31),
    (111, 0x44),
    (112, 0x4d),
    (113, 0x15),
    (114, 0x2d),
    (115, 0x1b),
    (116, 0x2c),
    (117, 0x3c),
    (118, 0x2a),
    (119, 0x1d),
    (120, 0x22),
    (121, 0x35),
    (122, 0x1a),
    (127, 0xe071),
    (273, 0xe075),
    (274, 0xe072),
    (275, 0xe074),
    (276, 0xe06b),
    (277, 0xe070),
    (278, 0xe06c),
    (279, 0xe069),
    (280, 0xe07d),
    (281, 0xe07a),
    (282, 0x05),
    (283, 0x06),
    (284, 0x04),
    (285, 0x0c),
    (286, 0x03),
    (287, 0x0b),
    (288, 0x83),
    (289, 0x0a),
    (290, 0x01),
    (291, 0x09),
    (292, 0x78),
    (293, 0x07),
    (301, 0x58),
    (303, 0x59),
    (304, 0x12),
    (305, 0xe014),
    (306, 0x14),
    (307, 0xe011),
    (308, 0x11),
];

struct VdpApi {
    _library: Library,
    setup: VoidFn,
    run_loop: VoidFn,
    signal_vblank: VoidFn,
    shutdown: VoidFn,
    copy_framebuffer: CopyFramebufferFn,
    send_byte: SendByteFn,
    receive_byte: ReceiveByteFn,
    is_cts: CtsFn,
    get_audio: AudioFn,
    send_ps2_key: SendPs2KeyFn,
}

impl VdpApi {
    unsafe fn symbol<T: Copy>(library: &Library, name: &[u8]) -> Result<T, String> {
        unsafe { library.get::<T>(name) }
            .map(|symbol| *symbol)
            .map_err(|error| {
                format!(
                    "missing VDP symbol {}: {error}",
                    String::from_utf8_lossy(&name[..name.len() - 1])
                )
            })
    }

    fn bundled_path() -> Result<PathBuf, String> {
        let directory = std::env::temp_dir().join("agon-libretro");
        std::fs::create_dir_all(&directory).map_err(|error| {
            format!(
                "could not create bundled VDP directory {}: {error}",
                directory.display()
            )
        })?;
        let filename = format!(
            "vdp_console8-{}.{}",
            env!("AGON_BUNDLED_VDP_HASH"),
            Path::new(VDP_FILENAME)
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("so")
        );
        let path = directory.join(filename);
        let current_size = std::fs::metadata(&path).map(|metadata| metadata.len()).ok();
        if current_size != Some(BUNDLED_VDP.len() as u64) {
            std::fs::write(&path, BUNDLED_VDP).map_err(|error| {
                format!(
                    "could not extract bundled VDP to {}: {error}",
                    path.display()
                )
            })?;
        }
        Ok(path)
    }

    fn candidates() -> Result<Vec<PathBuf>, String> {
        let mut paths = Vec::new();
        if let Some(path) = std::env::var_os("AGON_LIBRETRO_VDP") {
            paths.push(PathBuf::from(path));
        }
        if let Some(system_directory) = libretro_backend::system_directory() {
            paths.push(system_directory.join("agon").join(VDP_FILENAME));
        }
        if let Ok(executable) = std::env::current_exe() {
            if let Some(directory) = executable.parent() {
                paths.push(directory.join(VDP_FILENAME));
                paths.push(directory.join("cores").join(VDP_FILENAME));
            }
        }
        paths.push(Self::bundled_path()?);
        Ok(paths)
    }

    fn load() -> Result<Self, String> {
        let mut errors = Vec::new();
        for path in Self::candidates()? {
            let library = match unsafe { Library::new(&path) } {
                Ok(library) => library,
                Err(error) => {
                    errors.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            return unsafe {
                Ok(Self {
                    setup: Self::symbol(&library, b"vdp_setup\0")?,
                    run_loop: Self::symbol(&library, b"vdp_loop\0")?,
                    signal_vblank: Self::symbol(&library, b"signal_vblank\0")?,
                    shutdown: Self::symbol(&library, b"vdp_shutdown\0")?,
                    copy_framebuffer: Self::symbol(&library, b"copyVgaFramebuffer\0")?,
                    send_byte: Self::symbol(&library, b"z80_send_to_vdp\0")?,
                    receive_byte: Self::symbol(&library, b"z80_recv_from_vdp\0")?,
                    is_cts: Self::symbol(&library, b"z80_uart0_is_cts\0")?,
                    get_audio: Self::symbol(&library, b"getAudioSamples\0")?,
                    send_ps2_key: Self::symbol(&library, b"sendPS2KbEventToFabgl\0")?,
                    _library: library,
                })
            };
        }
        Err(format!(
            "could not load Agon VDP firmware:\n{}",
            errors.join("\n")
        ))
    }
}

struct VdpState {
    api: Arc<VdpApi>,
    thread: Option<JoinHandle<()>>,
}

impl VdpState {
    fn start() -> Result<Self, String> {
        let api = Arc::new(VdpApi::load()?);
        let setup = api.setup;
        let run_loop = api.run_loop;
        let thread = std::thread::Builder::new()
            .name("Agon VDP".to_owned())
            .spawn(move || unsafe {
                setup();
                run_loop();
            })
            .map_err(|error| format!("could not start Agon VDP: {error}"))?;
        std::thread::sleep(std::time::Duration::from_millis(500));
        Ok(Self {
            api,
            thread: Some(thread),
        })
    }
}

impl Drop for VdpState {
    fn drop(&mut self) {
        unsafe { (self.api.shutdown)() };
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // The userspace VDP starts detached C++ workers. Keep their module loaded
        // after shutdown so the frontend can safely unload this libretro core.
        std::mem::forget(self.api.clone());
    }
}

#[derive(Clone)]
struct VdpSerial(Arc<VdpApi>);

impl SerialLink for VdpSerial {
    fn send(&mut self, byte: u8) {
        unsafe { (self.0.send_byte)(byte) };
    }

    fn recv(&mut self) -> Option<u8> {
        let mut byte = 0;
        unsafe { (self.0.receive_byte)(&mut byte) }.then_some(byte)
    }

    fn read_clear_to_send(&mut self) -> bool {
        unsafe { (self.0.is_cts)() }
    }
}

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

struct RunningMachine {
    machine: AgonMachine,
    cpu: ez80::Cpu,
    _shutdown: Arc<AtomicBool>,
    _soft_reset: Arc<AtomicBool>,
}

pub struct AgonLibretroCore {
    running: Option<RunningMachine>,
    vdp: Option<VdpState>,
    framebuffer: Vec<u16>,
    vdp_framebuffer: Vec<u8>,
    audio_u8: Vec<u8>,
    audio_i16: Vec<i16>,
    staged_game: Option<PathBuf>,
    loaded_game: Option<GameData>,
    pressed_keys: HashSet<u32>,
}

impl Default for AgonLibretroCore {
    fn default() -> Self {
        Self {
            running: None,
            vdp: None,
            framebuffer: vec![0; WIDTH * HEIGHT],
            vdp_framebuffer: vec![0; 1024 * 768 * 3],
            audio_u8: vec![0; AUDIO_FRAMES_PER_VIDEO_FRAME],
            audio_i16: vec![0; AUDIO_FRAMES_PER_VIDEO_FRAME * 2],
            staged_game: None,
            loaded_game: None,
            pressed_keys: HashSet::new(),
        }
    }
}

impl AgonLibretroCore {
    fn firmware_path() -> PathBuf {
        if let Some(path) = std::env::var_os("AGON_LIBRETRO_FIRMWARE") {
            return PathBuf::from(path);
        }
        if let Some(system_directory) = libretro_backend::system_directory() {
            let path = system_directory.join("agon").join("mos_console8.bin");
            if path.is_file() {
                return path;
            }
        }

        let directory = std::env::temp_dir()
            .join("agon-libretro")
            .join(env!("CARGO_PKG_VERSION"));
        std::fs::create_dir_all(&directory).unwrap_or_else(|error| {
            panic!(
                "could not create bundled MOS directory {}: {error}",
                directory.display()
            )
        });
        let mos_path = directory.join("mos_console8.bin");
        let map_path = directory.join("mos_console8.map");
        for (path, bytes) in [(&mos_path, BUNDLED_MOS), (&map_path, BUNDLED_MOS_MAP)] {
            let current_size = std::fs::metadata(path).map(|metadata| metadata.len()).ok();
            if current_size != Some(bytes.len() as u64) {
                std::fs::write(path, bytes).unwrap_or_else(|error| {
                    panic!(
                        "could not extract bundled MOS file {}: {error}",
                        path.display()
                    )
                });
            }
        }
        mos_path
    }

    fn sdcard_dir(content_path: Option<&Path>) -> PathBuf {
        if let Some(path) = std::env::var_os("AGON_LIBRETRO_SDCARD") {
            return PathBuf::from(path);
        }
        if let Some(parent) = content_path.and_then(Path::parent) {
            return parent.to_path_buf();
        }
        if let Some(system_directory) = libretro_backend::system_directory() {
            let agon_directory = system_directory.join("agon");
            if agon_directory.is_dir() {
                return agon_directory;
            }
        }
        PathBuf::from("sdcard")
    }

    fn reset_machine(&mut self, content: Option<&Path>) {
        let vdp = self.vdp.as_ref().expect("VDP is loaded").api.clone();
        let (tx_gpio, _rx_gpio) = mpsc::channel();
        let gpios = Arc::new(gpio::GpioSet::new());
        let shutdown = Arc::new(AtomicBool::new(false));
        let soft_reset = Arc::new(AtomicBool::new(false));
        let mut machine = AgonMachine::new(AgonMachineConfig {
            uart0_link: Box::new(VdpSerial(vdp)),
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
        self.running = Some(RunningMachine {
            machine,
            cpu,
            _shutdown: shutdown,
            _soft_reset: soft_reset,
        });
    }

    fn copy_video(&mut self) {
        let Some(vdp) = &self.vdp else {
            return;
        };
        let mut width = 0;
        let mut height = 0;
        let mut frame_rate = 60.0;
        unsafe {
            (vdp.api.copy_framebuffer)(
                &mut width,
                &mut height,
                self.vdp_framebuffer.as_mut_ptr(),
                &mut frame_rate,
            );
        }
        self.framebuffer.fill(0);
        let copy_width = (width as usize).min(WIDTH);
        let copy_height = (height as usize).min(HEIGHT);
        for y in 0..copy_height {
            for x in 0..copy_width {
                let source = (y * width as usize + x) * 3;
                let red = self.vdp_framebuffer[source] as u16;
                let green = self.vdp_framebuffer[source + 1] as u16;
                let blue = self.vdp_framebuffer[source + 2] as u16;
                self.framebuffer[y * WIDTH + x] =
                    ((red >> 3) << 11) | ((green >> 2) << 5) | (blue >> 3);
            }
        }
    }

    fn update_keyboard(&mut self, handle: &mut RuntimeHandle) {
        let Some(vdp) = &self.vdp else {
            return;
        };
        for &(keycode, scancode) in KEYBOARD_MAP {
            let is_down = handle.is_keyboard_key_pressed(keycode);
            let was_down = self.pressed_keys.contains(&keycode);
            if is_down == was_down {
                continue;
            }
            unsafe { (vdp.api.send_ps2_key)(scancode, u8::from(is_down)) };
            if is_down {
                self.pressed_keys.insert(keycode);
            } else {
                self.pressed_keys.remove(&keycode);
            }
        }
    }

    fn copy_audio(&mut self) {
        let Some(vdp) = &self.vdp else {
            return;
        };
        unsafe {
            (vdp.api.get_audio)(self.audio_u8.as_mut_ptr(), self.audio_u8.len() as u32);
        }
        for (frame, sample) in self.audio_u8.iter().copied().enumerate() {
            let sample = ((sample as i16) - 127) << 8;
            self.audio_i16[frame * 2] = sample;
            self.audio_i16[frame * 2 + 1] = sample;
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
        match VdpState::start() {
            Ok(vdp) => self.vdp = Some(vdp),
            Err(error) => {
                eprintln!("{error}");
                return LoadGameResult::Failed(game_data);
            }
        }
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
        self.vdp = None;
        self.loaded_game
            .take()
            .expect("libretro unloaded a game that was never loaded")
    }

    fn on_run(&mut self, handle: &mut RuntimeHandle) {
        if let (Some(running), Some(vdp)) = (&mut self.running, &self.vdp) {
            unsafe { (vdp.api.signal_vblank)() };
            running.machine.gpios().b.set_input_pin(1, true);
            running.machine.gpios().b.set_input_pin(1, false);
            running
                .machine
                .run_for_cycles(&mut running.cpu, CLOCKS_PER_FRAME);
        }
        self.update_keyboard(handle);
        self.copy_video();
        self.copy_audio();
        handle.upload_audio_frame(&self.audio_i16);
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
