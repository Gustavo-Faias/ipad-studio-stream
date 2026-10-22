//! Diagnóstico do usbmuxd.
//!
//! ```text
//! cargo run -p padstudio-usbmux --example devices                  # lista os dispositivos
//! cargo run -p padstudio-usbmux --example devices -- watch 30      # eventos durante 30 s
//! cargo run -p padstudio-usbmux --example devices -- wait 300      # espera (até 300 s) que se ligue um dispositivo USB
//! cargo run -p padstudio-usbmux --example devices -- info          # túnel até ao lockdownd: nome, modelo, versão
//! cargo run -p padstudio-usbmux --example devices -- connect 23456 # testa uma porta do dispositivo
//! ```

use std::process::ExitCode;
use std::time::{Duration, Instant};

use padstudio_usbmux::{Device, DeviceEvent, UsbmuxAddr, UsbmuxClient, UsbmuxError, lockdown};

type Result<T> = std::result::Result<T, UsbmuxError>;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let number = |default: u64| args.get(1).and_then(|s| s.parse().ok()).unwrap_or(default);
    let result = match args.first().map(String::as_str).unwrap_or("list") {
        "list" => list(),
        "watch" => watch(Duration::from_secs(number(30))),
        "wait" => wait(Duration::from_secs(number(300))),
        "info" => info(),
        "connect" => connect(number(u64::from(padstudio_usbmux_port())) as u16),
        other => {
            eprintln!("comando desconhecido: {other} (usa list, watch [s], info, connect [porta])");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("erro: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Porta PSWP do iPad (sem depender do crate do protocolo neste exemplo).
fn padstudio_usbmux_port() -> u16 {
    23456
}

fn describe(d: &Device) -> String {
    let bus = match d.speed_bps {
        Some(s) if s >= 5_000_000_000 => format!(", USB 3 ({} Gb/s)", s / 1_000_000_000),
        Some(s) if s >= 480_000_000 => format!(", USB 2 ({} Mb/s)", s / 1_000_000),
        Some(s) => format!(", {} Mb/s", s / 1_000_000),
        None => String::new(),
    };
    format!("#{} {} · UDID {} · {:?}{bus}", d.device_id, d.kind(), d.udid, d.connection)
}

fn usb_devices() -> Result<Vec<Device>> {
    Ok(UsbmuxClient::connect()?.list_devices()?.into_iter().filter(Device::is_usb).collect())
}

fn list() -> Result<()> {
    println!("usbmuxd em {}", UsbmuxAddr::from_env_or_default());
    let devices = UsbmuxClient::connect()?.list_devices()?;
    if devices.is_empty() {
        println!("nenhum dispositivo ligado (liga o cabo e desbloqueia o iPad)");
    }
    for d in &devices {
        println!("  {}", describe(d));
    }
    Ok(())
}

fn watch(duration: Duration) -> Result<()> {
    println!("a escutar eventos durante {} s (liga/desliga o cabo)…", duration.as_secs());
    let mut events = UsbmuxClient::connect()?.listen()?;
    events.set_read_timeout(Some(Duration::from_millis(500)))?;
    let start = Instant::now();
    while start.elapsed() < duration {
        let t = start.elapsed().as_secs_f64();
        match events.next_event()? {
            Some(DeviceEvent::Attached(d)) => println!("{t:6.2}s  ligado     {}", describe(&d)),
            Some(DeviceEvent::Detached { device_id }) => println!("{t:6.2}s  desligado  #{device_id}"),
            Some(DeviceEvent::Paired { device_id }) => println!("{t:6.2}s  confiado   #{device_id}"),
            Some(DeviceEvent::Other(kind)) => println!("{t:6.2}s  evento {kind}"),
            None => {}
        }
    }
    Ok(())
}

/// Termina com sucesso assim que houver um dispositivo ligado por USB.
fn wait(timeout: Duration) -> Result<()> {
    println!("à espera de um dispositivo USB (até {} s)…", timeout.as_secs());
    let mut events = UsbmuxClient::connect()?.listen()?; // os já ligados chegam logo como Attached
    events.set_read_timeout(Some(Duration::from_millis(500)))?;
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(DeviceEvent::Attached(d)) = events.next_event()?
            && d.is_usb() {
                println!("{:.1} s: ligado {}", start.elapsed().as_secs_f64(), describe(&d));
                return Ok(());
            }
    }
    Err(UsbmuxError::NoDevice(None))
}

fn info() -> Result<()> {
    let devices = usb_devices()?;
    if devices.is_empty() {
        return Err(UsbmuxError::NoDevice(None));
    }
    for d in devices {
        println!("{}", describe(&d));
        let started = Instant::now();
        let info = lockdown::query(d.device_id)?;
        println!("  túnel até ao lockdownd OK ({:.1} ms) · serviço {}", started.elapsed().as_secs_f64() * 1000.0, info.service_type);
        let show = |label: &str, v: &Option<String>| println!("  {label:<16} {}", v.as_deref().unwrap_or("(recusado sem emparelhamento)"));
        show("Nome", &info.device_name);
        show("Modelo", &info.product_type);
        show("Sistema", &info.product_version);
        show("Classe", &info.device_class);
        show("Hardware", &info.hardware_model);
    }
    Ok(())
}

fn connect(port: u16) -> Result<()> {
    let devices = usb_devices()?;
    let d = devices.first().ok_or(UsbmuxError::NoDevice(None))?;
    println!("{}", describe(d));
    match UsbmuxClient::connect()?.connect_device(d.device_id, port) {
        Ok(_tunnel) => println!("  porta {port}: aberta (túnel estabelecido)"),
        Err(UsbmuxError::ConnectionRefused { .. }) => println!("  porta {port}: recusada (nada à escuta no dispositivo)"),
        Err(e) => return Err(e),
    }
    Ok(())
}
