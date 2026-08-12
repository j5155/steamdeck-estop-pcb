//! SPDX-License-Identifier: MIT OR Apache-2.0
//!
//! Copyright (c) 2021–2024 The rp-rs Developers
//! Copyright (c) 2021 rp-rs organization
//! Copyright (c) 2025 Raspberry Pi Ltd.
//!
//! # GPIO 'Blinky' Example
//!
//! This application demonstrates how to control a GPIO pin on the rp2040 and rp235x.
//!
//! It may need to be adapted to your particular board layout and/or pin assignment.

#![no_std]
#![no_main]
extern crate alloc;

use defmt::*;
use defmt_rtt as _;
use embedded_hal::delay::DelayNs;
use embedded_hal::digital::{InputPin};

use usb_device as usbd;
use usbd::{
    class_prelude::UsbBusAllocator,
    device::{UsbDeviceBuilder, UsbVidPid},
};

use usbd_hid::{
    descriptor::{KeyboardReport, SerializedDescriptor},
    hid_class::{
        HIDClass, HidClassSettings, HidCountryCode, HidProtocol, HidSubClass,
        ProtocolModeConfig,
    },
};

use embedded_alloc::LlffHeap as Heap;

#[cfg(target_arch = "riscv32")]
use panic_halt as _;
#[cfg(target_arch = "arm")]
use panic_probe as _;

// Alias for our HAL crate
use hal::entry;

#[cfg(rp2350)]
use rp235x_hal as hal;

#[cfg(rp2040)]
use rp2040_hal as hal;
use rp2040_hal::fugit::{MillisDurationU32};
use rp2040_hal::pac;
use rp2040_hal::timer::Alarm;
use rp2040_hal::usb::UsbBus;
use usb_device::device::StringDescriptors;
use usb_device::LangID;
// use bsp::entry;
// use bsp::hal;
// use rp_pico as bsp;

/// The linker will place this boot block at the start of our program image. We
/// need this to help the ROM bootloader get our code up and running.
/// Note: This boot block is not necessary when using a rp-hal based BSP
/// as the BSPs already perform this step.
#[unsafe(link_section = ".boot2")]
#[used]
#[cfg(rp2040)]
pub static BOOT2: [u8; 256] = rp2040_boot2::BOOT_LOADER_W25Q080;


// https://hkubota.wordpress.com/2024/11/04/embassy-on-rpi-pico-alloc/
#[global_allocator]
static HEAP: Heap = Heap::empty();

/// Tell the Boot ROM about our application
#[unsafe(link_section = ".start_block")]
#[used]
#[cfg(rp2350)]
pub static IMAGE_DEF: hal::block::ImageDef = hal::block::ImageDef::secure_exe();

/// External high-speed crystal on the Raspberry Pi Pico 2 board is 12 MHz.
/// Adjust if your board has a different frequency
const XTAL_FREQ_HZ: u32 = 12_000_000u32;

/// Entry point to our bare-metal application.
///
/// The `#[hal::entry]` macro ensures the Cortex-M start-up code calls this function
/// as soon as all global variables and the spinlock are initialised.
///
/// The function configures the rp2040 and rp235x peripherals, then toggles a GPIO pin in
/// an infinite loop. If there is an LED connected to that pin, it will blink.
#[entry]
fn main() -> ! {
    info!("Program start");
    // Grab our singleton objects
    let mut pac = pac::Peripherals::take().unwrap();

    // Set up the watchdog driver - needed by the clock setup code
    let mut watchdog = hal::Watchdog::new(pac.WATCHDOG);

    // Configure the clocks
    let clocks = hal::clocks::init_clocks_and_plls(
        XTAL_FREQ_HZ,
        pac.XOSC,
        pac.CLOCKS,
        pac.PLL_SYS,
        pac.PLL_USB,
        &mut pac.RESETS,
        &mut watchdog,
    )
    .unwrap();

    #[cfg(rp2040)]
    let mut timer = hal::Timer::new(pac.TIMER, &mut pac.RESETS, &clocks);

    #[cfg(rp2350)]
    let mut timer = hal::Timer::new_timer0(pac.TIMER0, &mut pac.RESETS, &clocks);

    // https://github.com/KOBA789/rusty-keys/blob/main/firmware/keyboard/src/bin/simple.rs

    let p = pac::Peripherals::take().unwrap();

    let bus = UsbBus::new(
        p.USBCTRL_REGS,
        p.USBCTRL_DPRAM,
        clocks.usb_clock,
        true,
        &mut pac.RESETS,
    );

    let bus_allocator = UsbBusAllocator::new(bus);

    // https://github.com/raspberrypi/usb-pid
    // raspberry pi's VID, PID of random generic HID device ("Spectra GmbH & Co. KG CDC / HID device")
    let vid_pid = UsbVidPid(0x2E8A, 0x104E);

    let mut hid = HIDClass::new_with_settings(
        &bus_allocator,
        KeyboardReport::desc(),
        10,
        HidClassSettings {
            subclass: HidSubClass::NoSubClass,
            protocol: HidProtocol::Keyboard,
            config: ProtocolModeConfig::ForceReport,
            locale: HidCountryCode::NotSupported,
        },
    );
    let string_descriptors: &[StringDescriptors<'_>] = &[StringDescriptors::new(LangID::EN_US)
        .manufacturer("TODO PICK A NAME") // todo pick a name
        .product("Steam Deck ESTOP Button")
        .serial_number("12087")
    ];
    let mut dev = UsbDeviceBuilder::new(&bus_allocator, vid_pid).strings(string_descriptors).unwrap().build();

    // The single-cycle I/O block controls our GPIO pins
    let sio = hal::Sio::new(pac.SIO);

    // Set the pins to their default state
    let pins = hal::gpio::Pins::new(
        pac.IO_BANK0,
        pac.PADS_BANK0,
        sio.gpio_bank0,
        &mut pac.RESETS,
    );

    // Configure GPIO25 as an output
    /*
    let mut led_pin = pins.gpio25.into_push_pull_output();
    loop {
        info!("on!");
        led_pin.set_high().unwrap();
        timer.delay_ms(200);
        info!("off!");
        led_pin.set_low().unwrap();
        timer.delay_ms(200);
    }

     */
    let estop_pin = pins.gpio0.into_pull_up_input();
    let mut last_state = false;

    let mut alarm0 = timer.alarm_0().unwrap();
    let mut alarm1 = timer.alarm_1().unwrap();
    let mut alarm0_started = false;
    loop {
        dev.poll(&mut [&mut hid]);

        let state = estop_pin.as_input().is_low().unwrap();
        if state != last_state {
            // debounce 10 milliseconds
            // TODO is this duration or code right??
            if !alarm0.finished() && !alarm0_started {
                alarm0.schedule(MillisDurationU32::millis(10).convert()).expect("Alarm 0 failed!");
                alarm0_started = true;
                continue;
            }
            alarm0_started = false;

            if state {
                hid.push_input(&KeyboardReport {
                    modifier: 0,
                    reserved: 0,
                    leds: 0,
                    keycodes: [44u8; 6], // https://superuser.com/questions/1616334/what-is-the-scancode-for-spacebar
                }).unwrap();

                // Start timer for clearing
                // 1 second
                alarm1.schedule(MillisDurationU32::millis(1000).convert()).expect("Alarm 1 failed!");
            } else {
                hid.push_input(&KeyboardReport::default()).unwrap();

                alarm1.cancel().unwrap();
            }
            last_state = state;
        }

        if state && alarm1.finished() {
            hid.push_input(&KeyboardReport {
                modifier: 0,
                reserved: 0,
                leds: 0,
                // ESC + I
                // https://gist.github.com/mildsunrise/4e231346e2078f440969cdefb6d4caa3
                // there's probably a better way to write this
                keycodes: [41u8, 12u8, 12u8, 12u8, 12u8, 12u8],
            }).expect("Keyboard pushed input error!");
        }

        timer.delay_ms(1); // avoid busy looping
    }
}

/// Program metadata for `picotool info`
#[unsafe(link_section = ".bi_entries")]
#[used]
pub static PICOTOOL_ENTRIES: [hal::binary_info::EntryAddr; 5] = [
    hal::binary_info::rp_cargo_bin_name!(),
    hal::binary_info::rp_cargo_version!(),
    hal::binary_info::rp_program_description!(c"Blinky Example"),
    hal::binary_info::rp_cargo_homepage_url!(),
    hal::binary_info::rp_program_build_attribute!(),
];

// End of file
