//! Register map of the BCM5720 (NetXtreme, 5717/5719/5720 family).
//!
//! Hardware facts only: offsets, bit values and magic numbers. Sources, read
//! to understand the chip and not copied: FreeBSD `sys/dev/bge/if_bgereg.h`
//! (@ 142cba95, BSD) and Linux `drivers/net/ethernet/broadcom/tg3.h` (GPL,
//! used as a datasheet only). Values marked "measured" were read on a real
//! BCM5720 (Dell R630) before this driver existed.

// ── BAR0: the PCI configuration shadow (0x00..0xFF) ───────────────────────────
/// Vendor (low 16) and device (high 16) id.
pub const PCI_ID: u32 = 0x00;
/// PCI command (low 16) and status (high 16), as the chip sees them.
pub const PCI_CMD: u32 = 0x04;
pub const PCI_CMD_MEM: u32 = 1 << 1;
pub const PCI_CMD_MASTER: u32 = 1 << 2;
/// Miscellaneous host control (`TG3PCI_MISC_HOST_CTRL` / `BGE_PCI_MISC_CTL`).
pub const MISC_HOST_CTRL: u32 = 0x68;
pub const MHC_CLEAR_INT: u32 = 1 << 0;
pub const MHC_MASK_PCI_INT: u32 = 1 << 1;
pub const MHC_WORD_SWAP: u32 = 1 << 3;
pub const MHC_PCISTATE_RW: u32 = 1 << 4;
pub const MHC_INDIRECT_ACCESS: u32 = 1 << 7;
/// Tagged status: the chip stamps every status block with a tag, the driver
/// acknowledges by writing it back (bge: MSI on the 5717 family needs it).
pub const MHC_TAGGED_STATUS: u32 = 1 << 9;
/// What the driver keeps in MISC_HOST_CTRL: interrupts masked at the PCI
/// level, host word swap, PCISTATE writable, indirect access (bge `BGE_INIT`).
pub const MHC_INIT: u32 = MHC_MASK_PCI_INT | MHC_WORD_SWAP | MHC_PCISTATE_RW | MHC_INDIRECT_ACCESS | MHC_TAGGED_STATUS;
/// DMA read/write control.
pub const DMA_RW_CTRL: u32 = 0x6C;
pub const DMA_RW_DIS_CACHE_ALIGNMENT: u32 = 1 << 0;
pub const DMA_RW_TAGGED_STATUS_WA: u32 = 1 << 7;
/// PCI state (ROM and APE access enables).
pub const PCISTATE: u32 = 0x70;
pub const PCISTATE_ROM_ENABLE: u32 = 1 << 5;
pub const PCISTATE_ROM_RETRY_ENABLE: u32 = 1 << 6;
pub const PCISTATE_ALLOW_APE: u32 = 0x0007_0000; // control space, shared memory, scratchpad
/// Base of the 32 KiB NIC SRAM window at BAR0 + `SRAM_WINDOW`.
pub const MEM_WIN_BASE: u32 = 0x7C;
/// Gen-2 product id / ASIC revision (5717, 5718, 5719, 5720 device ids).
pub const PRODID_ASICREV: u32 = 0xF4;
/// The chip id of the 5720 A0 (measured: `0x05720000`).
pub const CHIPID_5720_A0: u32 = 0x0572_0000;
pub const ASICREV_5720: u32 = 0x5720;

// ── Mailboxes (BAR0 0x200..0x3FF, 64-bit, the low word at +4) ────────────────
pub const MBOX_IRQ0: u32 = 0x204;
pub const MBOX_RX_STD_PROD: u32 = 0x26C;
pub const MBOX_RX_JUMBO_PROD: u32 = 0x274;
pub const MBOX_RX_CONS0: u32 = 0x284;
pub const MBOX_TX_HOST_PROD0: u32 = 0x304;

// ── MAC ──────────────────────────────────────────────────────────────────────
pub const MAC_MODE: u32 = 0x400;
pub const MAC_MODE_HALF_DUPLEX: u32 = 1 << 1;
pub const MAC_MODE_PORT_MASK: u32 = 0x0000_000C;
pub const MAC_MODE_PORT_MII: u32 = 0x4;
pub const MAC_MODE_PORT_GMII: u32 = 0x8;
/// Internal MAC loopback.
pub const MAC_MODE_INT_LOOPBACK: u32 = 1 << 4;
pub const MAC_MODE_RX_STATS_ENB: u32 = 1 << 11;
pub const MAC_MODE_RX_STATS_CLEAR: u32 = 1 << 12;
pub const MAC_MODE_TX_STATS_ENB: u32 = 1 << 14;
pub const MAC_MODE_TX_STATS_CLEAR: u32 = 1 << 15;
pub const MAC_MODE_TXDMA_ENB: u32 = 1 << 21;
pub const MAC_MODE_RXDMA_ENB: u32 = 1 << 22;
pub const MAC_MODE_FRMHDR_DMA_ENB: u32 = 1 << 23;
pub const MAC_MODE_APE_RX_EN: u32 = 1 << 27;
pub const MAC_MODE_APE_TX_EN: u32 = 1 << 28;
pub const MAC_STATUS: u32 = 0x404;
pub const MAC_STATUS_CLEAR: u32 = 0x0040_1018; // MI complete, link, sync, config changed
pub const MAC_EVENT_ENB: u32 = 0x408;
pub const MAC_EVENT_LINK_CHANGED: u32 = 1 << 12;
pub const MAC_ADDR0_HI: u32 = 0x410;
pub const MAC_ADDR0_LO: u32 = 0x414;
pub const TX_RANDOM_BACKOFF: u32 = 0x438;
pub const RX_MTU: u32 = 0x43C;
pub const MI_COMM: u32 = 0x44C;
pub const MI_COMM_READ: u32 = 0x0800_0000;
pub const MI_COMM_WRITE: u32 = 0x0400_0000;
pub const MI_COMM_BUSY: u32 = 0x2000_0000;
pub const MI_COMM_READ_FAIL: u32 = 0x1000_0000;
pub const MI_STATUS: u32 = 0x450;
pub const MI_MODE: u32 = 0x454;
pub const MI_MODE_AUTOPOLL: u32 = 1 << 4;
pub const TX_MODE: u32 = 0x45C;
pub const TX_MODE_ENABLE: u32 = 1 << 1;
pub const TX_MODE_MBUF_LOCKUP_FIX: u32 = 1 << 8;
/// 5720: preserved from the bootcode (jumbo frame length, count-down mode).
pub const TX_MODE_KEEP_5720: u32 = 0x00C0_0000;
pub const MAC_TX_STATUS: u32 = 0x460;
pub const TX_STATUS_LINK_UP: u32 = 1 << 3;
pub const TX_LENGTHS: u32 = 0x464;
/// IPG CRS 2, IPG 6, slot time 32 (`0x2620`).
pub const TX_LENGTHS_DEFAULT: u32 = 0x2620;
/// 5720: preserved from the bootcode (jumbo frame length, count-down value).
pub const TX_LENGTHS_KEEP_5720: u32 = 0xFFFF_0000;
pub const RX_MODE: u32 = 0x468;
pub const RX_MODE_ENABLE: u32 = 1 << 1;
pub const RX_MODE_PROMISC: u32 = 1 << 8;
pub const RX_RULES_CFG: u32 = 0x500;
pub const MAX_RX_FRAME_LOWAT: u32 = 0x504;

// ── Send / receive state machines ───────────────────────────────────────────
pub const SDI_MODE: u32 = 0x0C00;
pub const SDI_STATS_CTL: u32 = 0x0C08;
pub const SDI_STATS_ENABLE_MASK: u32 = 0x0C0C;
pub const SDC_MODE: u32 = 0x1000;
pub const SRS_MODE: u32 = 0x1400;
pub const SBDI_MODE: u32 = 0x1800;
pub const SBDC_MODE: u32 = 0x1C00;
pub const RXLP_MODE: u32 = 0x2000;
pub const RXLP_CFG: u32 = 0x2010;
pub const RXLP_STATS_CTL: u32 = 0x2014;
pub const RXLP_STATS_ENABLE_MASK: u32 = 0x2018;
pub const RDBDI_MODE: u32 = 0x2400;
pub const RX_JUMBO_RCB_MAXLEN_FLAGS: u32 = 0x2448;
pub const RX_STD_RCB_HADDR_HI: u32 = 0x2450;
pub const RX_STD_RCB_HADDR_LO: u32 = 0x2454;
pub const RX_STD_RCB_MAXLEN_FLAGS: u32 = 0x2458;
pub const RX_STD_RCB_NICADDR: u32 = 0x245C;
pub const RDC_MODE: u32 = 0x2800;
pub const RBDI_MODE: u32 = 0x2C00;
pub const RBDI_STD_REPL_THRESH: u32 = 0x2C18;
pub const STD_REPLENISH_LWM: u32 = 0x2D00;
pub const JMB_REPLENISH_LWM: u32 = 0x2D04;
pub const RBDC_MODE: u32 = 0x3000;
/// Every block's mode register: bit 1 enables it.
pub const MODE_ENABLE: u32 = 1 << 1;
pub const MODE_ATTN: u32 = 1 << 2;

// ── CPMU ─────────────────────────────────────────────────────────────────────
pub const CPMU_CLCK_ORIDE: u32 = 0x3624;
pub const CPMU_CLCK_ORIDE_MAC: u32 = 1 << 31;
/// CPMU status: bits 31:30 hold the PCI function on the 5719/5720.
pub const CPMU_STATUS: u32 = 0x362C;

// ── Host coalescing ──────────────────────────────────────────────────────────
pub const HCC_MODE: u32 = 0x3C00;
pub const HCC_MODE_NOW: u32 = 1 << 3;
pub const HCC_MODE_STATBLK_32: u32 = 0x100;
pub const HCC_RX_COAL_TICKS: u32 = 0x3C08;
pub const HCC_TX_COAL_TICKS: u32 = 0x3C0C;
pub const HCC_RX_MAX_BDS: u32 = 0x3C10;
pub const HCC_TX_MAX_BDS: u32 = 0x3C14;
pub const HCC_RX_MAX_BDS_INT: u32 = 0x3C20;
pub const HCC_TX_MAX_BDS_INT: u32 = 0x3C24;
pub const HCC_STATUSBLK_HI: u32 = 0x3C38;
pub const HCC_STATUSBLK_LO: u32 = 0x3C3C;

// ── Memory arbiter, buffer manager, DMA engines ─────────────────────────────
pub const MARB_MODE: u32 = 0x4000;
pub const BMAN_MODE: u32 = 0x4400;
pub const BMAN_LOMBUF_ATTN: u32 = 1 << 4;
pub const BMAN_MBUF_READDMA_LOWAT: u32 = 0x4410;
pub const BMAN_MBUF_MACRX_LOWAT: u32 = 0x4414;
pub const BMAN_MBUF_HIWAT: u32 = 0x4418;
pub const BMAN_DESC_LOWAT: u32 = 0x4434;
pub const BMAN_DESC_HIWAT: u32 = 0x4438;
pub const RDMA_MODE: u32 = 0x4800;
pub const RDMA_ALL_ATTNS: u32 = 0x0000_03FC;
pub const RDMA_FIFO_LONG_BURST: u32 = 0x0003_0000;
pub const RDMA_MULT_DMA_RD_DIS: u32 = 1 << 24;
pub const RDMA_H2BNC_VLAN_DET: u32 = 1 << 29;
pub const RDMA_RSRVCTRL: u32 = 0x4900;
pub const RDMA_RSRVCTRL_FIFO_OFLW_FIX: u32 = 1 << 2;
pub const RDMA_LSO_CRPTEN_CTRL: u32 = 0x4910;
pub const RDMA_LSO_BLEN_BD_512: u32 = 0x0002_0000;
pub const RDMA_LSO_BLEN_LSO_4K: u32 = 0x000C_0000;
pub const RDMA_TX_LENGTH_WA_5720: u32 = 0x0020_0000;
pub const RDMA_LENGTH: u32 = 0x4BE0;
pub const WDMA_MODE: u32 = 0x4C00;
pub const WDMA_ALL_ATTNS: u32 = 0x0000_03FC;
pub const WDMA_STATUS_TAG_FIX: u32 = 1 << 29;
pub const FTQ_RESET: u32 = 0x5C00;
/// Message-signalled interrupt mode.
pub const MSI_MODE: u32 = 0x6000;
pub const MSI_MODE_ONE_SHOT_DISABLE: u32 = 1 << 5;

// ── GRC (general control) ────────────────────────────────────────────────────
pub const GRC_MODE: u32 = 0x6800;
pub const GRC_WORDSWAP_NONFRAME: u32 = 1 << 2;
pub const GRC_BYTESWAP_DATA: u32 = 1 << 4;
pub const GRC_WORDSWAP_DATA: u32 = 1 << 5;
/// Little-endian host: the swap options every 57xx driver uses.
pub const GRC_SWAP_OPTIONS: u32 = GRC_WORDSWAP_NONFRAME | GRC_BYTESWAP_DATA | GRC_WORDSWAP_DATA;
/// 5720: host-to-BMC settings the APE firmware wrote; kept across our writes.
pub const GRC_KEEP_B2H: u32 = 0x0004_80C0;
pub const GRC_STACKUP: u32 = 1 << 16;
pub const GRC_HOST_SEND_BDS: u32 = 1 << 17;
pub const GRC_TX_NO_PHDR_CSUM: u32 = 1 << 20;
pub const GRC_MAC_ATTN_INTR: u32 = 1 << 26;
pub const GRC_MISC_CFG: u32 = 0x6804;
pub const MISC_CFG_CORE_RESET: u32 = 1 << 0;
/// Timer prescaler for the 66 MHz core clock (65 << 1).
pub const MISC_CFG_PRESCALER_66MHZ: u32 = 65 << 1;
/// Keeps the PCIe link from retraining during the core reset.
pub const MISC_CFG_NO_PCIE_RETRAIN: u32 = 1 << 29;
pub const GRC_LOCAL_CTRL: u32 = 0x6808;
pub const LOCAL_CTRL_INTR_ON_ATTN: u32 = 1 << 3;
pub const GRC_FASTBOOT_PC: u32 = 0x6894;

// ── NVRAM ────────────────────────────────────────────────────────────────────
pub const NVRAM_CMD: u32 = 0x7000;
pub const NVRAM_CMD_DONE: u32 = 1 << 3;
pub const NVRAM_CMD_READ: u32 = 0x0000_0198; // first | last | start | done
pub const NVRAM_ADDR: u32 = 0x700C;
pub const NVRAM_RDDATA: u32 = 0x7010;
pub const NVRAM_SWARB: u32 = 0x7020;
pub const NVRAM_SWARB_SET1: u32 = 1 << 1;
pub const NVRAM_SWARB_CLR1: u32 = 1 << 5;
pub const NVRAM_SWARB_GNT1: u32 = 1 << 9;
pub const NVRAM_ACCESS: u32 = 0x7024;
pub const NVRAM_ACCESS_ENABLE: u32 = 1 << 0;
/// First word of a valid NVRAM image.
pub const NVRAM_MAGIC: u32 = 0x6699_55AA;
/// Where function 0's MAC sits in NVRAM (function 1: 0xCC).
pub const NVRAM_MAC_FUNC0: u32 = 0x7C;
pub const NVRAM_MAC_FUNC1: u32 = 0xCC;

// ── NIC SRAM (through the window at BAR0 + 0x8000) ──────────────────────────
pub const SRAM_WINDOW: u32 = 0x8000;
pub const SRAM_WINDOW_LEN: u32 = 0x8000;
/// Ring control blocks: send rings at 0x100, receive return rings at 0x200.
pub const SRAM_SEND_RCB: u32 = 0x100;
pub const SRAM_RX_RETURN_RCB: u32 = 0x200;
/// Firmware mailbox: the driver writes `FW_MAGIC` before a reset, the
/// bootcode answers `!FW_MAGIC` when it is done.
pub const SRAM_FW_MBOX: u32 = 0xB50;
pub const SRAM_DATA_SIG: u32 = 0xB54;
pub const SRAM_DATA_CFG: u32 = 0xB58;
pub const SRAM_DATA_CFG_ASF: u32 = 1 << 7;
pub const SRAM_DRV_STATE: u32 = 0xC04;
pub const SRAM_MAC_HI: u32 = 0xC14;
pub const SRAM_MAC_LO: u32 = 0xC18;
/// 'KevT'.
pub const FW_MAGIC: u32 = 0x4B65_7654;
/// 'HK' in the top half of `SRAM_MAC_HI`: the bootcode put a MAC there.
pub const SRAM_MAC_SIG: u32 = 0x484B;
pub const DRV_STATE_START: u32 = 0x0000_0001;
pub const DRV_STATE_START_DONE: u32 = 0x8000_0001;
pub const DRV_STATE_UNLOAD: u32 = 0x0000_0002;
pub const DRV_STATE_UNLOAD_DONE: u32 = 0x8000_0002;
/// NIC addresses of the rings on the 5717/5719/5720.
pub const NIC_STD_RING_5717: u32 = 0x0004_0000;
pub const NIC_SEND_RING_5717: u32 = 0x0000_4000;

// ── Ring control block flags and descriptor flags ────────────────────────────
pub const RCB_FLAG_USE_EXT_RX_BD: u32 = 1 << 0;
pub const RCB_FLAG_DISABLED: u32 = 1 << 1;
pub const BD_FLAG_END: u32 = 1 << 2;
pub const RXBD_FLAG_ERROR: u32 = 1 << 10;
/// Error bits in the high half of a return descriptor's error/vlan word;
/// "odd nibble received on MII" is not an error.
pub const RXBD_ERR_ODD_NIBBLE: u32 = 0x0010;
/// Status block: the chip wrote a new status.
pub const STATUS_UPDATED: u32 = 1 << 0;
pub const STATUS_LINK_CHANGED: u32 = 1 << 1;

// ── APE (BAR2) ───────────────────────────────────────────────────────────────
pub const APE_EVENT: u32 = 0x000C;
pub const APE_EVENT_1: u32 = 1;
pub const APE_SEG_SIG: u32 = 0x4000;
/// 'APE!' (measured).
pub const APE_SEG_MAGIC: u32 = 0x4150_4521;
pub const APE_FW_STATUS: u32 = 0x400C;
pub const APE_FW_READY: u32 = 1 << 8;
pub const APE_FW_FEATURES: u32 = 0x4010;
pub const APE_FEATURE_NCSI: u32 = 1 << 1;
pub const APE_FEATURE_DASH: u32 = 1 << 0;
pub const APE_FW_VERSION: u32 = 0x4018;
pub const APE_HOST_SEG_SIG: u32 = 0x4200;
pub const APE_HOST_SEG_LEN: u32 = 0x4204;
pub const APE_HOST_INIT_COUNT: u32 = 0x4208;
pub const APE_HOST_DRIVER_ID: u32 = 0x420C;
pub const APE_HOST_BEHAVIOR: u32 = 0x4210;
pub const APE_HOST_HEARTBEAT_INT_MS: u32 = 0x4214;
pub const APE_HOST_DRVR_STATE: u32 = 0x421C;
/// 'HOST'.
pub const APE_HOST_SEG_MAGIC: u32 = 0x484F_5354;
pub const APE_HOST_SEG_LEN_MAGIC: u32 = 0x20;
pub const APE_HOST_BEHAV_NO_PHYLOCK: u32 = 1;
pub const APE_HOST_STATE_START: u32 = 1;
pub const APE_HOST_STATE_UNLOAD: u32 = 2;
pub const APE_EVENT_STATUS: u32 = 0x4300;
pub const APE_EVENT_DRIVER: u32 = 0x10;
pub const APE_EVENT_STATE_CHANGE: u32 = 0x500;
pub const APE_EVENT_STATE_START: u32 = 0x0001_0000;
pub const APE_EVENT_STATE_UNLOAD: u32 = 0x0002_0000;
pub const APE_EVENT_PENDING: u32 = 1 << 31;
/// Per-function lock request / grant registers (5717 and later), 4 bytes per lock.
pub const APE_LOCK_REQ: u32 = 0x8400;
pub const APE_LOCK_GRANT: u32 = 0x8420;
pub const APE_LOCK_PHY0: u32 = 0;
pub const APE_LOCK_GRC: u32 = 1;
pub const APE_LOCK_PHY1: u32 = 2;
pub const APE_LOCK_MEM: u32 = 4;
pub const APE_LOCK_GPIO: u32 = 7;
/// The lock bit of function 0 (and of every PHY lock).
pub const APE_LOCK_DRIVER0: u32 = 0x1000;

// ── PHY (MII, through MI_COMM) ───────────────────────────────────────────────
pub const MII_BMCR: u32 = 0x00;
pub const BMCR_SPEED1000: u16 = 1 << 6;
pub const BMCR_FULLDPLX: u16 = 1 << 8;
pub const BMCR_ANENABLE: u16 = 1 << 12;
pub const BMCR_LOOPBACK: u16 = 1 << 14;
pub const MII_BMSR: u32 = 0x01;
pub const BMSR_LSTATUS: u16 = 1 << 2;
pub const MII_PHYSID1: u32 = 0x02;
pub const MII_PHYSID2: u32 = 0x03;
/// Broadcom auxiliary status: speed/duplex in bits 10:8.
pub const MII_AUX_STAT: u32 = 0x19;
