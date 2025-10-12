// Take a look at the license at the top of the repository in the LICENSE file.

#![allow(clippy::too_many_arguments)]

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read};
use std::time::Instant;

use crate::sys::utils::to_u64;
use crate::{Cpu, CpuRefreshKind};

macro_rules! to_str {
    ($e:expr) => {
        unsafe { std::str::from_utf8_unchecked($e) }
    };
}

pub(crate) struct CpusWrapper {
    pub(crate) global_cpu: CpuUsage,
    pub(crate) cpus: Vec<Cpu>,
    got_cpu_frequency: bool,
    /// This field is needed to prevent updating when not enough time passed since last update.
    last_update: Option<Instant>,
}

impl CpusWrapper {
    pub(crate) fn new() -> Self {
        Self {
            global_cpu: CpuUsage::default(),
            cpus: Vec::with_capacity(4),
            got_cpu_frequency: false,
            last_update: None,
        }
    }

    pub(crate) fn refresh_if_needed(
        &mut self,
        only_update_global_cpu: bool,
        refresh_kind: CpuRefreshKind,
    ) {
        self.refresh(only_update_global_cpu, refresh_kind);
    }

    pub(crate) fn refresh(&mut self, only_update_global_cpu: bool, refresh_kind: CpuRefreshKind) {
        let need_cpu_usage_update = self
            .last_update
            .map(|last_update| last_update.elapsed() >= crate::MINIMUM_CPU_UPDATE_INTERVAL)
            .unwrap_or(true);

        let first = self.cpus.is_empty();
        let mut vendors_brands = if first {
            get_vendor_id_and_brand()
        } else {
            HashMap::new()
        };

        // If the last CPU usage update is too close (less than `MINIMUM_CPU_UPDATE_INTERVAL`),
        // we don't want to update CPUs times.
        if need_cpu_usage_update && (first || refresh_kind.cpu_usage()) {
/* Example /scheme/sys/stat output:
cpu  3655 0 10896 965406 37003
cpu0 344 0 626 29683 37003
cpu1 319 0 1632 28676 0
cpu2 227 0 1478 28920 0
cpu3 169 0 1125 29333 0
cpu4 139 0 740 29753 0
name user nice kernel idle irq
Description of fields above
*/

            let mut sys_stat = fs::read_to_string("/scheme/sys/stat").unwrap_or_default();
            self.last_update = Some(Instant::now());
            for line in sys_stat.lines() {
                let mut parts = line.split(' ').filter(|s| !s.is_empty());
                let name = parts.next().unwrap_or_default();
                if !name.starts_with("cpu") {
                    continue;
                }
                let user = parts.next().unwrap_or_default().parse::<u64>().unwrap_or_default();
                let nice = parts.next().unwrap_or_default().parse::<u64>().unwrap_or_default();
                let system = parts.next().unwrap_or_default().parse::<u64>().unwrap_or_default();
                let idle = parts.next().unwrap_or_default().parse::<u64>().unwrap_or_default();
                let iowait = 0;
                let irq = parts.next().unwrap_or_default().parse::<u64>().unwrap_or_default();
                let softirq = 0;
                let steal = 0;
                let guest = 0;
                let guest_nice = 0;

                // Global stats
                if name == "cpu" {
                    self.global_cpu.set(
                        user,
                        nice,
                        system,
                        idle,
                        iowait,
                        irq,
                        softirq,
                        steal,
                        guest,
                        guest_nice,
                    );
                    continue;
                }

                // Per-cpu stats
                let Ok(i) = name[3..].parse::<usize>() else { continue };
                if first {
                    let (vendor_id, brand) = match vendors_brands.remove(&i) {
                        Some((vendor_id, brand)) => (vendor_id, brand),
                        None => (String::new(), String::new()),
                    };
                    self.cpus.push(Cpu {
                        inner: CpuInner::new_with_values(
                            name,
                            user,
                            nice,
                            system,
                            idle,
                            iowait,
                            irq,
                            softirq,
                            steal,
                            guest,
                            guest_nice,
                            0,
                            vendor_id,
                            brand,
                        ),
                    });
                } else if let Some(cpu) = self.cpus.get_mut(i) {
                    cpu.inner.set(
                        user,
                        nice,
                        system,
                        idle,
                        iowait,
                        irq,
                        softirq,
                        steal,
                        guest,
                        guest_nice,
                    );
                }
            }
        }

        if refresh_kind.frequency() {
            //TODO: cpu frequency
        }
    }

    pub(crate) fn get_global_raw_times(&self) -> (u64, u64) {
        (self.global_cpu.total_time, self.global_cpu.old_total_time)
    }

    pub(crate) fn len(&self) -> usize {
        self.cpus.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.cpus.is_empty()
    }
}

/// Struct containing values to compute a CPU usage.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CpuValues {
    user: u64,
    nice: u64,
    system: u64,
    idle: u64,
    iowait: u64,
    irq: u64,
    softirq: u64,
    steal: u64,
    guest: u64,
    guest_nice: u64,
}

impl CpuValues {
    /// Sets the given argument to the corresponding fields.
    pub fn set(
        &mut self,
        user: u64,
        nice: u64,
        system: u64,
        idle: u64,
        iowait: u64,
        irq: u64,
        softirq: u64,
        steal: u64,
        guest: u64,
        guest_nice: u64,
    ) {
        // `guest` is already accounted in `user`.
        self.user = user.saturating_sub(guest);
        // `guest_nice` is already accounted in `nice`.
        self.nice = nice.saturating_sub(guest_nice);
        self.system = system;
        self.idle = idle;
        self.iowait = iowait;
        self.irq = irq;
        self.softirq = softirq;
        self.steal = steal;
        self.guest = guest;
        self.guest_nice = guest_nice;
    }

    #[inline]
    pub fn work_time(&self) -> u64 {
        self.user.saturating_add(self.nice)
    }

    #[inline]
    pub fn system_time(&self) -> u64 {
        self.system
            .saturating_add(self.irq)
            .saturating_add(self.softirq)
    }

    #[inline]
    pub fn idle_time(&self) -> u64 {
        self.idle.saturating_add(self.iowait)
    }

    #[inline]
    pub fn virtual_time(&self) -> u64 {
        self.guest.saturating_add(self.guest_nice)
    }

    #[inline]
    pub fn total_time(&self) -> u64 {
        self.work_time()
            .saturating_add(self.system_time())
            .saturating_add(self.idle_time())
            .saturating_add(self.virtual_time())
            .saturating_add(self.steal)
    }
}

#[derive(Default)]
pub(crate) struct CpuUsage {
    percent: f32,
    old_values: CpuValues,
    new_values: CpuValues,
    total_time: u64,
    old_total_time: u64,
}

impl CpuUsage {
    pub(crate) fn new_with_values(
        user: u64,
        nice: u64,
        system: u64,
        idle: u64,
        iowait: u64,
        irq: u64,
        softirq: u64,
        steal: u64,
        guest: u64,
        guest_nice: u64,
    ) -> Self {
        let mut new_values = CpuValues::default();
        new_values.set(
            user, nice, system, idle, iowait, irq, softirq, steal, guest, guest_nice,
        );
        Self {
            old_values: CpuValues::default(),
            new_values,
            percent: 0f32,
            total_time: 0,
            old_total_time: 0,
        }
    }

    pub(crate) fn set(
        &mut self,
        user: u64,
        nice: u64,
        system: u64,
        idle: u64,
        iowait: u64,
        irq: u64,
        softirq: u64,
        steal: u64,
        guest: u64,
        guest_nice: u64,
    ) {
        macro_rules! min {
            ($a:expr, $b:expr, $def:expr) => {
                if $a > $b { ($a - $b) as f32 } else { $def }
            };
        }

        self.old_values = self.new_values;
        self.new_values.set(
            user, nice, system, idle, iowait, irq, softirq, steal, guest, guest_nice,
        );

        self.total_time = self.new_values.total_time();
        self.old_total_time = self.old_values.total_time();

        let nice_period = self.new_values.nice.saturating_sub(self.old_values.nice);
        let user_period = self.new_values.user.saturating_sub(self.old_values.user);
        let steal_period = self.new_values.steal.saturating_sub(self.old_values.steal);
        let guest_period = self
            .new_values
            .virtual_time()
            .saturating_sub(self.old_values.virtual_time());
        let system_period = self
            .new_values
            .system_time()
            .saturating_sub(self.old_values.system_time());

        let total = min!(self.total_time, self.old_total_time, 1.);
        let nice = nice_period as f32 / total;
        let user = user_period as f32 / total;
        let system = system_period as f32 / total;
        let irq = (steal_period + guest_period) as f32 / total;

        self.percent = (nice + user + system + irq) * 100.;
        if self.percent > 100. {
            self.percent = 100.; // to prevent the percentage to go above 100%
        }
    }

    pub(crate) fn usage(&self) -> f32 {
        self.percent
    }
}

pub(crate) struct CpuInner {
    usage: CpuUsage,
    pub(crate) name: String,
    pub(crate) frequency: u64,
    pub(crate) vendor_id: String,
    pub(crate) brand: String,
}

impl CpuInner {
    pub(crate) fn new_with_values(
        name: &str,
        user: u64,
        nice: u64,
        system: u64,
        idle: u64,
        iowait: u64,
        irq: u64,
        softirq: u64,
        steal: u64,
        guest: u64,
        guest_nice: u64,
        frequency: u64,
        vendor_id: String,
        brand: String,
    ) -> Self {
        Self {
            usage: CpuUsage::new_with_values(
                user, nice, system, idle, iowait, irq, softirq, steal, guest, guest_nice,
            ),
            name: name.to_owned(),
            frequency,
            vendor_id,
            brand,
        }
    }

    pub(crate) fn set(
        &mut self,
        user: u64,
        nice: u64,
        system: u64,
        idle: u64,
        iowait: u64,
        irq: u64,
        softirq: u64,
        steal: u64,
        guest: u64,
        guest_nice: u64,
    ) {
        self.usage.set(
            user, nice, system, idle, iowait, irq, softirq, steal, guest, guest_nice,
        );
    }

    pub(crate) fn cpu_usage(&self) -> f32 {
        self.usage.percent
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Returns the CPU frequency in MHz.
    pub(crate) fn frequency(&self) -> u64 {
        self.frequency
    }

    pub(crate) fn vendor_id(&self) -> &str {
        &self.vendor_id
    }

    pub(crate) fn brand(&self) -> &str {
        &self.brand
    }
}

/// Obtain the implementer of this CPU core.
///
/// This has been obtained from util-linux's lscpu implementation, see
/// https://github.com/util-linux/util-linux/blob/7076703b529d255600631306419cca1b48ab850a/sys-utils/lscpu-arm.c#L240
///
/// This list will have to be updated every time a new vendor appears, please keep it synchronized
/// with util-linux and update the link above with the commit you have used.
fn get_arm_implementer(implementer: u32) -> Option<&'static str> {
    Some(match implementer {
        0x41 => "ARM",
        0x42 => "Broadcom",
        0x43 => "Cavium",
        0x44 => "DEC",
        0x46 => "FUJITSU",
        0x48 => "HiSilicon",
        0x49 => "Infineon",
        0x4d => "Motorola/Freescale",
        0x4e => "NVIDIA",
        0x50 => "APM",
        0x51 => "Qualcomm",
        0x53 => "Samsung",
        0x56 => "Marvell",
        0x61 => "Apple",
        0x66 => "Faraday",
        0x69 => "Intel",
        0x70 => "Phytium",
        0xc0 => "Ampere",
        _ => return None,
    })
}

/// Obtain the part of this CPU core.
///
/// This has been obtained from util-linux's lscpu implementation, see
/// https://github.com/util-linux/util-linux/blob/eb788e20b82d0e1001a30867c71c8bfb2bb86819/sys-utils/lscpu-arm.c#L25
///
/// This list will have to be updated every time a new core appears, please keep it synchronized
/// with util-linux and update the link above with the commit you have used.
fn get_arm_part(implementer: u32, part: u32) -> Option<&'static str> {
    Some(match (implementer, part) {
        // ARM
        (0x41, 0x810) => "ARM810",
        (0x41, 0x920) => "ARM920",
        (0x41, 0x922) => "ARM922",
        (0x41, 0x926) => "ARM926",
        (0x41, 0x940) => "ARM940",
        (0x41, 0x946) => "ARM946",
        (0x41, 0x966) => "ARM966",
        (0x41, 0xa20) => "ARM1020",
        (0x41, 0xa22) => "ARM1022",
        (0x41, 0xa26) => "ARM1026",
        (0x41, 0xb02) => "ARM11 MPCore",
        (0x41, 0xb36) => "ARM1136",
        (0x41, 0xb56) => "ARM1156",
        (0x41, 0xb76) => "ARM1176",
        (0x41, 0xc05) => "Cortex-A5",
        (0x41, 0xc07) => "Cortex-A7",
        (0x41, 0xc08) => "Cortex-A8",
        (0x41, 0xc09) => "Cortex-A9",
        (0x41, 0xc0d) => "Cortex-A17", // Originally A12
        (0x41, 0xc0f) => "Cortex-A15",
        (0x41, 0xc0e) => "Cortex-A17",
        (0x41, 0xc14) => "Cortex-R4",
        (0x41, 0xc15) => "Cortex-R5",
        (0x41, 0xc17) => "Cortex-R7",
        (0x41, 0xc18) => "Cortex-R8",
        (0x41, 0xc20) => "Cortex-M0",
        (0x41, 0xc21) => "Cortex-M1",
        (0x41, 0xc23) => "Cortex-M3",
        (0x41, 0xc24) => "Cortex-M4",
        (0x41, 0xc27) => "Cortex-M7",
        (0x41, 0xc60) => "Cortex-M0+",
        (0x41, 0xd01) => "Cortex-A32",
        (0x41, 0xd02) => "Cortex-A34",
        (0x41, 0xd03) => "Cortex-A53",
        (0x41, 0xd04) => "Cortex-A35",
        (0x41, 0xd05) => "Cortex-A55",
        (0x41, 0xd06) => "Cortex-A65",
        (0x41, 0xd07) => "Cortex-A57",
        (0x41, 0xd08) => "Cortex-A72",
        (0x41, 0xd09) => "Cortex-A73",
        (0x41, 0xd0a) => "Cortex-A75",
        (0x41, 0xd0b) => "Cortex-A76",
        (0x41, 0xd0c) => "Neoverse-N1",
        (0x41, 0xd0d) => "Cortex-A77",
        (0x41, 0xd0e) => "Cortex-A76AE",
        (0x41, 0xd13) => "Cortex-R52",
        (0x41, 0xd15) => "Cortex-R82",
        (0x41, 0xd16) => "Cortex-R52+",
        (0x41, 0xd20) => "Cortex-M23",
        (0x41, 0xd21) => "Cortex-M33",
        (0x41, 0xd22) => "Cortex-R55",
        (0x41, 0xd23) => "Cortex-R85",
        (0x41, 0xd40) => "Neoverse-V1",
        (0x41, 0xd41) => "Cortex-A78",
        (0x41, 0xd42) => "Cortex-A78AE",
        (0x41, 0xd43) => "Cortex-A65AE",
        (0x41, 0xd44) => "Cortex-X1",
        (0x41, 0xd46) => "Cortex-A510",
        (0x41, 0xd47) => "Cortex-A710",
        (0x41, 0xd48) => "Cortex-X2",
        (0x41, 0xd49) => "Neoverse-N2",
        (0x41, 0xd4a) => "Neoverse-E1",
        (0x41, 0xd4b) => "Cortex-A78C",
        (0x41, 0xd4c) => "Cortex-X1C",
        (0x41, 0xd4d) => "Cortex-A715",
        (0x41, 0xd4e) => "Cortex-X3",
        (0x41, 0xd4f) => "Neoverse-V2",
        (0x41, 0xd80) => "Cortex-A520",
        (0x41, 0xd81) => "Cortex-A720",
        (0x41, 0xd82) => "Cortex-X4",
        (0x41, 0xd84) => "Neoverse-V3",
        (0x41, 0xd85) => "Cortex-X925",
        (0x41, 0xd87) => "Cortex-A725",
        (0x41, 0xd8e) => "Neoverse-N3",

        // Broadcom
        (0x42, 0x00f) => "Brahma-B15",
        (0x42, 0x100) => "Brahma-B53",
        (0x42, 0x516) => "ThunderX2",

        // Cavium
        (0x43, 0x0a0) => "ThunderX",
        (0x43, 0x0a1) => "ThunderX-88XX",
        (0x43, 0x0a2) => "ThunderX-81XX",
        (0x43, 0x0a3) => "ThunderX-83XX",
        (0x43, 0x0af) => "ThunderX2-99xx",

        // DEC
        (0x44, 0xa10) => "SA110",
        (0x44, 0xa11) => "SA1100",

        // Fujitsu
        (0x46, 0x001) => "A64FX",

        // HiSilicon
        (0x48, 0xd01) => "Kunpeng-920", // aka tsv110

        // NVIDIA
        (0x4e, 0x000) => "Denver",
        (0x4e, 0x003) => "Denver 2",
        (0x4e, 0x004) => "Carmel",

        // APM
        (0x50, 0x000) => "X-Gene",

        // Qualcomm
        (0x51, 0x00f) => "Scorpion",
        (0x51, 0x02d) => "Scorpion",
        (0x51, 0x04d) => "Krait",
        (0x51, 0x06f) => "Krait",
        (0x51, 0x201) => "Kryo",
        (0x51, 0x205) => "Kryo",
        (0x51, 0x211) => "Kryo",
        (0x51, 0x800) => "Falkor-V1/Kryo",
        (0x51, 0x801) => "Kryo-V2",
        (0x51, 0x802) => "Kryo-3XX-Gold",
        (0x51, 0x803) => "Kryo-3XX-Silver",
        (0x51, 0x804) => "Kryo-4XX-Gold",
        (0x51, 0x805) => "Kryo-4XX-Silver",
        (0x51, 0xc00) => "Falkor",
        (0x51, 0xc01) => "Saphira",

        // Samsung
        (0x53, 0x001) => "exynos-m1",

        // Marvell
        (0x56, 0x131) => "Feroceon-88FR131",
        (0x56, 0x581) => "PJ4/PJ4b",
        (0x56, 0x584) => "PJ4B-MP",

        // Apple
        (0x61, 0x020) => "Icestorm-A14",
        (0x61, 0x021) => "Firestorm-A14",
        (0x61, 0x022) => "Icestorm-M1",
        (0x61, 0x023) => "Firestorm-M1",
        (0x61, 0x024) => "Icestorm-M1-Pro",
        (0x61, 0x025) => "Firestorm-M1-Pro",
        (0x61, 0x028) => "Icestorm-M1-Max",
        (0x61, 0x029) => "Firestorm-M1-Max",
        (0x61, 0x030) => "Blizzard-A15",
        (0x61, 0x031) => "Avalanche-A15",
        (0x61, 0x032) => "Blizzard-M2",
        (0x61, 0x033) => "Avalanche-M2",

        // Faraday
        (0x66, 0x526) => "FA526",
        (0x66, 0x626) => "FA626",

        // Intel
        (0x69, 0x200) => "i80200",
        (0x69, 0x210) => "PXA250A",
        (0x69, 0x212) => "PXA210A",
        (0x69, 0x242) => "i80321-400",
        (0x69, 0x243) => "i80321-600",
        (0x69, 0x290) => "PXA250B/PXA26x",
        (0x69, 0x292) => "PXA210B",
        (0x69, 0x2c2) => "i80321-400-B0",
        (0x69, 0x2c3) => "i80321-600-B0",
        (0x69, 0x2d0) => "PXA250C/PXA255/PXA26x",
        (0x69, 0x2d2) => "PXA210C",
        (0x69, 0x411) => "PXA27x",
        (0x69, 0x41c) => "IPX425-533",
        (0x69, 0x41d) => "IPX425-400",
        (0x69, 0x41f) => "IPX425-266",
        (0x69, 0x682) => "PXA32x",
        (0x69, 0x683) => "PXA930/PXA935",
        (0x69, 0x688) => "PXA30x",
        (0x69, 0x689) => "PXA31x",
        (0x69, 0xb11) => "SA1110",
        (0x69, 0xc12) => "IPX1200",

        // Phytium
        (0x70, 0x660) => "FTC660",
        (0x70, 0x661) => "FTC661",
        (0x70, 0x662) => "FTC662",
        (0x70, 0x663) => "FTC663",

        _ => return None,
    })
}

/// Returns the brand/vendor string for the first CPU (which should be the same for all CPUs).
pub(crate) fn get_vendor_id_and_brand() -> HashMap<usize, (String, String)> {
    let mut cpus = HashMap::new();
    let mut s = String::new();
    //TODO: allow reading information per CPU
    let Ok(s) = fs::read_to_string("/scheme/sys/cpu") else {
        return cpus;
    };
    let mut count = 1;
    let mut vendor = String::new();
    let mut model = String::new();
    for line in s.lines() {
        let mut parts = line.splitn(2, ": ");
        let Some(key) = parts.next() else { continue };
        let Some(value) = parts.next() else { continue };
        match key {
            "CPUs" => {
                value.parse::<usize>().map(|x| count = x);
            },
            "Vendor" => {
                vendor = value.to_string();
            },
            "Model" => {
                model = value.to_string();
            }
            _ => {}
        }
    }
    for id in 0..count {
        cpus.insert(id, (vendor.clone(), model.clone()));
    }
    cpus
}