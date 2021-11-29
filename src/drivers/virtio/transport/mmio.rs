//! A module containing all virtio specific pci functionality
//!
//! The module contains ...
#![allow(dead_code)]

use crate::arch::mm::PhysAddr;
#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::_mm_mfence;
use core::convert::TryInto;
use core::ptr::{read_volatile, write_volatile};
use core::result::Result;
use core::u8;

#[cfg(target_arch = "riscv64")]
use crate::arch::riscv::kernel::irq::*;
#[cfg(target_arch = "x86_64")]
use crate::arch::x86_64::kernel::irq::*;
use crate::drivers::error::DriverError;
use crate::drivers::net::network_irqhandler;
use crate::drivers::net::virtio_net::VirtioNetDriver;
use crate::drivers::virtio::device;
use crate::drivers::virtio::error::VirtioError;

/// Virtio device ID's
/// See Virtio specification v1.1. - 5
///
// WARN: Upon changes in the set of the enum variants
// one MUST adjust the associated From<u32>
// implementation, in order catch all cases correctly,
// as this function uses the catch-all "_" case!
#[allow(non_camel_case_types)]
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
#[repr(u32)]
pub enum DevId {
	INVALID = 0x0,
	VIRTIO_DEV_ID_NET = 1,
	VIRTIO_DEV_ID_BLK = 2,
	VIRTIO_DEV_ID_CONSOLE = 3,
}

impl From<DevId> for u32 {
	fn from(val: DevId) -> u32 {
		match val {
			DevId::VIRTIO_DEV_ID_NET => 1,
			DevId::VIRTIO_DEV_ID_BLK => 2,
			DevId::VIRTIO_DEV_ID_CONSOLE => 3,
			DevId::INVALID => 0x0,
		}
	}
}

impl From<u32> for DevId {
	fn from(val: u32) -> Self {
		match val {
			1 => DevId::VIRTIO_DEV_ID_NET,
			2 => DevId::VIRTIO_DEV_ID_BLK,
			3 => DevId::VIRTIO_DEV_ID_CONSOLE,
			_ => DevId::INVALID,
		}
	}
}

pub struct VqCfgHandler<'a> {
	vq_index: u32,
	raw: &'a mut MmioRegisterLayout,
}

impl<'a> VqCfgHandler<'a> {
	/// Sets the size of a given virtqueue. In case the provided size exceeds the maximum allowed
	/// size, the size is set to this maximum instead. Else size is set to the provided value.
	///
	/// Returns the set size in form of a `u16`.
	pub fn set_vq_size(&mut self, size: u16) -> u16 {
		self.raw
			.set_queue_size(self.vq_index, size as u32)
			.try_into()
			.unwrap()
	}

	pub fn set_ring_addr(&mut self, addr: PhysAddr) {
		self.raw.set_ring_addr(self.vq_index, addr);
	}

	pub fn set_drv_ctrl_addr(&mut self, addr: PhysAddr) {
		self.raw.set_drv_ctrl_addr(self.vq_index, addr);
	}

	pub fn set_dev_ctrl_addr(&mut self, addr: PhysAddr) {
		self.raw.set_dev_ctrl_addr(self.vq_index, addr);
	}

	pub fn notif_off(&mut self) -> u16 {
		// we don't need an offset
		0
	}

	pub fn enable_queue(&mut self) {
		self.raw.enable_queue(self.vq_index);
	}
}

/// Wraps a [ComCfgRaw](structs.comcfgraw.html) in order to preserve
/// the original structure.
///
/// Provides a safe API for Raw structure and allows interaction with the device via
/// the structure.
pub struct ComCfg {
	// References the raw structure in PCI memory space. Is static as
	// long as the device is present, which is mandatory in order to let this code work.
	com_cfg: &'static mut MmioRegisterLayout,

	/// Preferences of the device for this config. From 1 (highest) to 2^7-1 (lowest)
	rank: u8,
}

// Public Interface of ComCfg
impl ComCfg {
	pub fn new(raw: &'static mut MmioRegisterLayout, rank: u8) -> Self {
		ComCfg { com_cfg: raw, rank }
	}

	/// Select a queue via an index. If queue does NOT exist returns `None`, else
	/// returns `Some(VqCfgHandler)`.
	///
	/// INFO: The queue size is automatically bounded by constant `src::config:VIRTIO_MAX_QUEUE_SIZE`.
	pub fn select_vq(&mut self, index: u16) -> Option<VqCfgHandler<'_>> {
		if self.com_cfg.get_max_queue_size(u32::from(index)) == 0 {
			None
		} else {
			Some(VqCfgHandler {
				vq_index: index as u32,
				raw: self.com_cfg,
			})
		}
	}

	pub fn get_max_queue_size(&mut self, sel: u32) -> u32 {
		self.com_cfg.get_max_queue_size(sel)
	}

	pub fn is_queue_ready(&mut self, sel: u32) -> bool {
		self.com_cfg.is_queue_ready(sel)
	}

	/// Returns the device status field.
	pub fn dev_status(&self) -> u8 {
		self.com_cfg.status.try_into().unwrap()
	}

	/// Resets the device status field to zero.
	pub fn reset_dev(&mut self) {
		#[cfg(target_arch = "riscv64")]
		unsafe {
			write_volatile(&mut self.com_cfg.status, 0);
		}

		#[cfg(target_arch = "x86_64")]
		unsafe {
			self.com_cfg.status = 0;
			_mm_mfence();
		}
	}

	/// Sets the device status field to FAILED.
	/// A driver MUST NOT initialize and use the device any further after this.
	/// A driver MAY use the device again after a proper reset of the device.
	pub fn set_failed(&mut self) {
		#[cfg(target_arch = "riscv64")]
		unsafe {
			write_volatile(&mut self.com_cfg.status, u32::from(device::Status::FAILED));
		}

		#[cfg(target_arch = "x86_64")]
		unsafe {
			self.com_cfg.status = u32::from(device::Status::FAILED);
			_mm_mfence();
		}
	}

	/// Sets the ACKNOWLEDGE bit in the device status field. This indicates, the
	/// OS has notived the device
	pub fn ack_dev(&mut self) {
		unsafe {
			let status = read_volatile(&self.com_cfg.status);

			#[cfg(target_arch = "riscv64")]
			{
				write_volatile(
					&mut self.com_cfg.status,
					status | u32::from(device::Status::ACKNOWLEDGE),
				);
			}

			#[cfg(target_arch = "x86_64")]
			{
				_mm_mfence();
				self.com_cfg.status = status | u32::from(device::Status::ACKNOWLEDGE);
			}
		}
	}

	/// Sets the DRIVER bit in the device status field. This indicates, the OS
	/// know how to run this device.
	pub fn set_drv(&mut self) {
		unsafe {
			let status = read_volatile(&self.com_cfg.status);

			#[cfg(target_arch = "riscv64")]
			{
				write_volatile(
					&mut self.com_cfg.status,
					status | u32::from(device::Status::DRIVER),
				);
			}

			#[cfg(target_arch = "x86_64")]
			{
				_mm_mfence();
				self.com_cfg.status = status | u32::from(device::Status::DRIVER);
			}
		}
	}

	/// Sets the FEATURES_OK bit in the device status field.
	///
	/// Drivers MUST NOT accept new features after this step.
	pub fn features_ok(&mut self) {
		unsafe {
			let status = read_volatile(&self.com_cfg.status);

			#[cfg(target_arch = "riscv64")]
			{
				write_volatile(
					&mut self.com_cfg.status,
					status | u32::from(device::Status::FEATURES_OK),
				);
			}

			#[cfg(target_arch = "x86_64")]
			{
				_mm_mfence();
				self.com_cfg.status = status | u32::from(device::Status::FEATURES_OK);
			}
		}
	}

	/// In order to correctly check feature negotiaten, this function
	/// MUST be called after [self.features_ok()](ComCfg::features_ok()) in order to check
	/// if features have been accepted by the device after negotiation.
	///
	/// Re-reads device status to ensure the FEATURES_OK bit is still set:
	/// otherwise, the device does not support our subset of features and the device is unusable.
	pub fn check_features(&self) -> bool {
		unsafe {
			let status = read_volatile(&self.com_cfg.status);
			#[cfg(target_arch = "x86_64")]
			_mm_mfence();
			status & u32::from(device::Status::FEATURES_OK)
				== u32::from(device::Status::FEATURES_OK)
		}
	}

	/// Sets the DRIVER_OK bit in the device status field.
	///
	/// After this call, the device is "live"!
	pub fn drv_ok(&mut self) {
		unsafe {
			let status = read_volatile(&self.com_cfg.status);

			#[cfg(target_arch = "riscv64")]
			{
				write_volatile(
					&mut self.com_cfg.status,
					status | u32::from(device::Status::DRIVER_OK),
				);
			}

			#[cfg(target_arch = "x86_64")]
			{
				_mm_mfence();
				self.com_cfg.status = status | u32::from(device::Status::DRIVER_OK);
			}
		}
	}

	/// Returns the features offered by the device. Coded in a 64bit value.
	pub fn dev_features(&mut self) -> u64 {
		self.com_cfg.dev_features()
	}

	/// Write selected features into driver_select field.
	pub fn set_drv_features(&mut self, feats: u64) {
		self.com_cfg.set_drv_features(feats);
	}

	pub fn print_information(&mut self) {
		self.com_cfg.print_information();
	}
}

/// Notification Structure to handle virtqueue notification settings.
/// See Virtio specification v1.1 - 4.1.4.4
pub struct NotifCfg {
	/// Start addr, from where the notification addresses for the virtqueues are computed
	queue_notify: *mut u32,
}

impl NotifCfg {
	pub fn new(registers: &mut MmioRegisterLayout) -> Self {
		let raw = &mut registers.queue_notify as *mut u32;

		NotifCfg { queue_notify: raw }
	}

	/// Returns base address of notification area as an usize
	pub fn base(&self) -> usize {
		self.queue_notify as usize
	}

	/// Returns the multiplier, needed in order to calculate the
	/// notification address for a specific queue.
	pub fn multiplier(&self) -> u32 {
		// we don't need a multiplier
		0
	}
}

/// Control structure, allowing to notify a device via PCI bus.
/// Typcially hold by a virtqueue.
pub struct NotifCtrl {
	/// Indicates if VIRTIO_F_NOTIFICATION_DATA has been negotiated
	f_notif_data: bool,
	/// Where to write notification
	notif_addr: *mut u32,
}

impl NotifCtrl {
	/// Retunrs a new controller. By default MSI-X capabilities and VIRTIO_F_NOTIFICATION_DATA
	/// are disabled.
	pub fn new(notif_addr: *mut usize) -> Self {
		NotifCtrl {
			f_notif_data: false,
			notif_addr: notif_addr as *mut u32,
		}
	}

	/// Enables VIRTIO_F_NOTIFICATION_DATA. This changes which data is provided to the device. ONLY a good idea if Feature has been negotiated.
	pub fn enable_notif_data(&mut self) {
		self.f_notif_data = true;
	}

	pub fn notify_dev(&self, notif_data: &[u8]) {
		let data = u32::from_ne_bytes(notif_data.try_into().unwrap());
		unsafe {
			*self.notif_addr = data;
		}
	}
}

/// Wraps a [IsrStatusRaw](structs.isrstatusraw.html) in order to preserve
/// the original structure and allow interaction with the device via
/// the structure.
///
/// Provides a safe API for Raw structure and allows interaction with the device via
/// the structure.
pub struct IsrStatus {
	raw: &'static mut IsrStatusRaw,
}

impl IsrStatus {
	pub fn new(registers: &mut MmioRegisterLayout) -> Self {
		let ptr = &mut registers.interrupt_status as *mut _;
		let raw: &'static mut IsrStatusRaw = unsafe { &mut *(ptr as *mut IsrStatusRaw) };

		IsrStatus { raw: raw }
	}

	pub fn is_interrupt(&self) -> bool {
		unsafe {
			let status = read_volatile(&self.raw.interrupt_status);
			#[cfg(target_arch = "x86_64")]
			_mm_mfence();
			status & 0x1 == 0x1
		}
	}

	pub fn is_cfg_change(&self) -> bool {
		unsafe {
			let status = read_volatile(&self.raw.interrupt_status);
			#[cfg(target_arch = "x86_64")]
			_mm_mfence();
			status & 0x2 == 0x2
		}
	}

	pub fn acknowledge(&mut self) {
		unsafe {
			let status = read_volatile(&self.raw.interrupt_status);

			#[cfg(target_arch = "riscv64")]
			{
				write_volatile(&mut self.raw.interrupt_ack, status);
			}

			#[cfg(target_arch = "x86_64")]
			{
				_mm_mfence();
				self.raw.interrupt_ack = status;
			}
		}
	}
}

#[repr(C)]
struct IsrStatusRaw {
	interrupt_status: u32,
	interrupt_ack: u32,
}

pub enum VirtioDriver {
	Network(VirtioNetDriver),
}

pub fn init_device(
	registers: &'static mut MmioRegisterLayout,
	irq_no: u32,
) -> Result<VirtioDriver, DriverError> {
	let dev_id: u16 = 0;

	if registers.version == 0x1 {
		error!("Legacy interface isn't supported!");
		return Err(DriverError::InitVirtioDevFail(
			VirtioError::DevNotSupported(dev_id),
		));
	}

	// Verify the device-ID to find the network card
	match registers.device_id {
		DevId::VIRTIO_DEV_ID_NET => {
			match VirtioNetDriver::init(dev_id, registers, irq_no) {
				Ok(virt_net_drv) => {
					info!("Virtio network driver initialized.");
					// Install interrupt handler
					irq_install_handler(irq_no.try_into().unwrap(), network_irqhandler as usize);
					#[cfg(target_arch = "x86_64")]
					add_irq_name(irq_no, "virtio_net");

					Ok(VirtioDriver::Network(virt_net_drv))
				}
				Err(virtio_error) => {
					error!("Virtio network driver could not be initialized with device");
					Err(DriverError::InitVirtioDevFail(virtio_error))
				}
			}
		}
		_ => {
			error!(
				"Device with id {:?} is currently not supported!",
				registers.device_id
			);
			// Return Driver error inidacting device is not supported
			Err(DriverError::InitVirtioDevFail(
				VirtioError::DevNotSupported(dev_id),
			))
		}
	}
}

/// The Layout of MMIO Device
#[repr(C, align(4))]
pub struct MmioRegisterLayout {
	magic_value: u32,
	version: u32,
	device_id: DevId,
	vendor_id: u32,

	device_features: u32,
	device_features_sel: u32,
	_reserved0: [u32; 2],
	driver_features: u32,
	driver_features_sel: u32,

	guest_page_size: u32, // legacy only
	_reserved1: u32,

	queue_sel: u32,
	queue_num_max: u32,
	queue_num: u32,
	queue_align: u32, // legacy only
	queue_pfn: u32,   // legacy only
	queue_ready: u32, // non-legacy only
	_reserved2: [u32; 2],
	queue_notify: u32,
	_reserved3: [u32; 3],

	interrupt_status: u32,
	interrupt_ack: u32,
	_reserved4: [u32; 2],

	status: u32,
	_reserved5: [u32; 3],

	queue_desc_low: u32,  // non-legacy only
	queue_desc_high: u32, // non-legacy only
	_reserved6: [u32; 2],
	queue_driver_low: u32,  // non-legacy only
	queue_driver_high: u32, // non-legacy only
	_reserved7: [u32; 2],
	queue_device_low: u32,  // non-legacy only
	queue_device_high: u32, // non-legacy only
	_reserved8: [u32; 21],

	config_generation: u32, // non-legacy only
	config: [u32; 3],
}

impl MmioRegisterLayout {
	pub fn get_magic_value(&self) -> u32 {
		self.magic_value
	}

	pub fn get_version(&self) -> u32 {
		self.version
	}

	pub fn get_device_id(&self) -> DevId {
		self.device_id
	}

	pub fn enable_queue(&mut self, sel: u32) {
		#[cfg(target_arch = "riscv64")]
		unsafe {
			write_volatile(&mut self.queue_sel, sel);
			write_volatile(&mut self.queue_ready, 1);
		}

		#[cfg(target_arch = "x86_64")]
		unsafe {
			self.queue_sel = sel;
			_mm_mfence();
			self.queue_ready = 1;
		}
	}

	#[cfg(target_arch = "x86_64")]
	pub fn get_max_queue_size(&mut self, sel: u32) -> u32 {
		self.queue_sel = sel;
		unsafe {
			_mm_mfence();
		}
		self.queue_num_max
	}

	#[cfg(target_arch = "riscv64")]
	pub fn get_max_queue_size(&mut self, sel: u32) -> u32 {
		unsafe {
			write_volatile(&mut self.queue_sel, sel);
			read_volatile(&self.queue_num_max)
		}
	}

	#[cfg(target_arch = "x86_64")]
	pub fn set_queue_size(&mut self, sel: u32, size: u32) -> u32 {
		self.queue_sel = sel;
		unsafe {
			_mm_mfence();
		}
		let num_max = self.queue_num_max;

		if num_max >= size {
			self.queue_num = size;
			size
		} else {
			self.queue_num = num_max;
			num_max
		}
	}

	#[cfg(target_arch = "riscv64")]
	pub fn set_queue_size(&mut self, sel: u32, size: u32) -> u32 {
		unsafe {
			write_volatile(&mut self.queue_sel, sel);

			let num_max = read_volatile(&self.queue_num_max);

			if num_max >= size {
				write_volatile(&mut self.queue_num, size);
				size
			} else {
				write_volatile(&mut self.queue_num, num_max);
				num_max
			}
		}
	}

	#[cfg(target_arch = "x86_64")]
	pub fn set_ring_addr(&mut self, sel: u32, addr: PhysAddr) {
		self.queue_sel = sel;
		unsafe {
			_mm_mfence();
		}
		self.queue_desc_low = addr.as_u64() as u32;
		self.queue_desc_high = (addr.as_u64() >> 32) as u32;
	}

	#[cfg(target_arch = "riscv64")]
	pub fn set_ring_addr(&mut self, sel: u32, addr: PhysAddr) {
		unsafe {
			write_volatile(&mut self.queue_sel, sel);
			write_volatile(&mut self.queue_desc_low, addr.as_u64() as u32);
			write_volatile(&mut self.queue_desc_high, (addr.as_u64() >> 32) as u32);
		}
	}

	#[cfg(target_arch = "x86_64")]
	pub fn set_drv_ctrl_addr(&mut self, sel: u32, addr: PhysAddr) {
		self.queue_sel = sel;
		unsafe {
			_mm_mfence();
		}
		self.queue_driver_low = addr.as_u64() as u32;
		self.queue_driver_high = (addr.as_u64() >> 32) as u32;
	}

	#[cfg(target_arch = "riscv64")]
	pub fn set_drv_ctrl_addr(&mut self, sel: u32, addr: PhysAddr) {
		unsafe {
			write_volatile(&mut self.queue_sel, sel);
			write_volatile(&mut self.queue_driver_low, addr.as_u64() as u32);
			write_volatile(&mut self.queue_driver_high, (addr.as_u64() >> 32) as u32);
		}
	}

	#[cfg(target_arch = "x86_64")]
	pub fn set_dev_ctrl_addr(&mut self, sel: u32, addr: PhysAddr) {
		self.queue_sel = sel;
		unsafe {
			_mm_mfence();
		}
		self.queue_device_low = addr.as_u64() as u32;
		self.queue_device_high = (addr.as_u64() >> 32) as u32;
	}

	#[cfg(target_arch = "riscv64")]
	pub fn set_dev_ctrl_addr(&mut self, sel: u32, addr: PhysAddr) {
		unsafe {
			write_volatile(&mut self.queue_sel, sel);
			write_volatile(&mut self.queue_device_low, addr.as_u64() as u32);
			write_volatile(&mut self.queue_device_high, (addr.as_u64() >> 32) as u32);
		}
	}

	#[cfg(target_arch = "x86_64")]
	pub fn is_queue_ready(&mut self, sel: u32) -> bool {
		self.queue_sel = sel;
		unsafe {
			_mm_mfence();
		}
		self.queue_ready != 0
	}

	#[cfg(target_arch = "riscv64")]
	pub fn is_queue_ready(&mut self, sel: u32) -> bool {
		unsafe {
			write_volatile(&mut self.queue_sel, sel);
			read_volatile(&self.queue_ready) != 0
		}
	}

	#[cfg(target_arch = "x86_64")]
	pub fn dev_features(&mut self) -> u64 {
		// Indicate device to show high 32 bits in device_feature field.
		// See Virtio specification v1.1. - 4.1.4.3
		self.device_features_sel = 1;
		unsafe {
			_mm_mfence();
		}

		// read high 32 bits of device features
		let mut dev_feat = u64::from(self.device_features) << 32;
		unsafe {
			_mm_mfence();
		}

		// Indicate device to show low 32 bits in device_feature field.
		// See Virtio specification v1.1. - 4.1.4.3
		self.device_features_sel = 0;
		unsafe {
			_mm_mfence();
		}

		// read low 32 bits of device features
		dev_feat |= u64::from(self.device_features);

		dev_feat
	}

	#[cfg(target_arch = "riscv64")]
	pub fn dev_features(&mut self) -> u64 {
		unsafe {
			// Indicate device to show high 32 bits in device_feature field.
			// See Virtio specification v1.1. - 4.1.4.3
			write_volatile(&mut self.device_features_sel, 1);

			// read high 32 bits of device features
			let mut dev_feat = u64::from(read_volatile(&self.device_features)) << 32;

			// Indicate device to show low 32 bits in device_feature field.
			// See Virtio specification v1.1. - 4.1.4.3
			write_volatile(&mut self.device_features_sel, 0);

			// read low 32 bits of device features
			dev_feat |= u64::from(read_volatile(&self.device_features));

			dev_feat
		}
	}

	/// Write selected features into driver_select field.
	#[cfg(target_arch = "x86_64")]
	pub fn set_drv_features(&mut self, feats: u64) {
		let high: u32 = (feats >> 32) as u32;
		let low: u32 = feats as u32;

		// Indicate to device that driver_features field shows low 32 bits.
		// See Virtio specification v1.1. - 4.1.4.3
		self.driver_features_sel = 0;
		unsafe {
			_mm_mfence();
		}

		// write low 32 bits of device features
		self.driver_features = low;
		unsafe {
			_mm_mfence();
		}

		// Indicate to device that driver_features field shows high 32 bits.
		// See Virtio specification v1.1. - 4.1.4.3
		self.driver_features_sel = 1;
		unsafe {
			_mm_mfence();
		}

		// write high 32 bits of device features
		self.driver_features = high;
	}

	#[cfg(target_arch = "riscv64")]
	pub fn set_drv_features(&mut self, feats: u64) {
		let high: u32 = (feats >> 32) as u32;
		let low: u32 = feats as u32;

		unsafe {
			// Indicate to device that driver_features field shows low 32 bits.
			// See Virtio specification v1.1. - 4.1.4.3
			write_volatile(&mut self.driver_features_sel, 0);

			// write low 32 bits of device features
			write_volatile(&mut self.driver_features, low);

			// Indicate to device that driver_features field shows high 32 bits.
			// See Virtio specification v1.1. - 4.1.4.3
			write_volatile(&mut self.driver_features_sel, 1);

			// write high 32 bits of device features
			write_volatile(&mut self.driver_features, high);
		}
	}

	pub fn get_config(&mut self) -> [u32; 3] {
		// see Virtio specification v1.1 -  2.4.1
		unsafe {
			loop {
				let before = read_volatile(&self.config_generation);
				#[cfg(target_arch = "x86_64")]
				_mm_mfence();
				let config = read_volatile(&self.config);
				#[cfg(target_arch = "x86_64")]
				_mm_mfence();
				let after = read_volatile(&self.config_generation);

				if before == after {
					return config;
				}
			}
		}
	}

	pub fn print_information(&mut self) {
		infoheader!(" MMIO RREGISTER LAYOUT INFORMATION ");

		infoentry!("Device version", "{:#X}", self.version);
		infoentry!("Device ID", "{:?}", self.device_id);
		infoentry!("Vendor ID", "{:#X}", self.vendor_id);
		infoentry!("Device Features", "{:#X}", self.dev_features());
		infoentry!("Interrupt status", "{:#X}", self.interrupt_status);
		infoentry!("Device status", "{:#X}", self.status);
		infoentry!("Configuration space", "{:#X?}", self.get_config());

		infofooter!();
	}
}
