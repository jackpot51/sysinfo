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