//! `[net-up]`: the network of an in-kernel NIC comes up in the background
//! (fast boot, 2026-11).
//!
//! The boot used to do DHCP, two ARP lookups, a ping, a DNS query, an HTTP and
//! an HTTPS fetch and IPv6 router discovery inline, before the scheduler, each
//! a bounded spin over the NIC: 2.3 s of the boot under KVM, 9 s under TCG,
//! more on a network that answers slowly or not at all. The self-test image
//! keeps that path (its markers are what the test matrix compares). The
//! release image initializes the NIC at the same place and leaves the rest to
//! this kernel task (`sched::spawn_kernel`, like the network worker):
//!
//! 1. DHCP DISCOVER/OFFER/REQUEST/ACK. No answer: retry with a growing pause
//!    (1 s to 10 s), for as long as the machine runs, so a cable plugged in
//!    later or a slow DHCP server still gets a lease.
//! 2. ARP for the gateway and the DNS server.
//! 3. IPv6 router solicitation, a short wait for the advertisement.
//! 4. `save` the configuration, start the network worker, tell the desktop.
//!
//! Until step 4 the netstack is unconfigured (`get()` is None), so nobody else
//! reads the NIC: `rx_route`, `rx_route_irq` and `service` all return early.
//! This task therefore polls `nic::poll_recv` itself and needs no lock against
//! the desktop loop. It never spins: between two looks at the NIC it sleeps a
//! tick. After step 4 it parks for good and the NIC belongs to the receive
//! routing as on every other path.
//!
//! The gateway and the DNS server are the ones the lease names (the inline
//! path assumes QEMU's 10.0.2.2 / 10.0.2.3).
//!
//! Not for the userspace driver (`net.driver=user`: `udrv::boot` configures
//! the stack) and not for a USB NIC (`late_bring_up`).

use super::*;
use core::sync::atomic::AtomicU8;
use euronet::dhcp;

/// 0 = not started, 1 = waiting for a lease, 2 = up (netstat-free state bit).
static STATE: AtomicU8 = AtomicU8::new(0);
/// Set once when the network came up; the desktop loop takes it.
static UP_EVENT: AtomicBool = AtomicBool::new(false);
/// Tick at which the task started.
static T_START: AtomicU64 = AtomicU64::new(0);

const CHAN: u64 = 0x4E45_5455_5021_2121; // "NETUP!!!"

/// Start the bring-up task. False when it runs already, when the NIC is not an
/// in-kernel one, or when the task table is full.
pub fn start() -> bool {
    if nic::frames_from_process() || nic::mac().is_none() || get().is_some() {
        return false;
    }
    if STATE.swap(1, Ordering::AcqRel) != 0 {
        return false;
    }
    T_START.store(crate::interrupts::ticks(), Ordering::Relaxed);
    // nice +5: the desktop comes first; the task mostly sleeps anyway.
    match crate::sched::spawn_kernel(task_main, 5) {
        Some(t) => {
            crate::serial_println!(
                "[net-up] kernel task {t}: DHCP + ARP + router discovery over {} run in the background",
                nic::kind()
            );
            true
        }
        None => {
            STATE.store(0, Ordering::Release);
            crate::serial_println!("[net-up] no free task slot: the network stays down (`net` in the shell) → FAILED");
            false
        }
    }
}

/// True once, after the network came up in the background (desktop loop).
pub fn take_up_event() -> bool {
    UP_EVENT.swap(false, Ordering::AcqRel)
}

fn sleep(ticks: u64) {
    {
        let _g = crate::sched::IfOffGuard::new();
        crate::sched::sleep_ticks(ticks);
    }
    crate::sched::yield_now();
}

/// Look at what the NIC received, one tick at a time, for at most `ticks`:
/// the first frame `want` accepts ends the wait.
fn wait_frame<T>(ticks: u64, mut want: impl FnMut(&[u8]) -> Option<T>) -> Option<T> {
    let end = crate::interrupts::ticks() + ticks;
    loop {
        for _ in 0..64 {
            let Some(rx) = nic::poll_recv() else { break };
            if let Some(v) = want(&rx) {
                return Some(v);
            }
        }
        if crate::interrupts::ticks() >= end {
            return None;
        }
        sleep(1);
    }
}

fn dhcp_reply(rx: &[u8], xid_type: u8, xid: u32, mac: [u8; 6]) -> Option<dhcp::DhcpInfo> {
    let (h, p) = EthernetHeader::parse(rx).ok()?;
    if h.ethertype != EtherType::Ipv4 {
        return None;
    }
    let (iph, ipl) = Ipv4Header::parse(p).ok()?;
    if iph.protocol != Protocol::Udp || ipl.len() <= 8 || u16::from_be_bytes([ipl[2], ipl[3]]) != 68 {
        return None;
    }
    dhcp::parse(&ipl[8..]).filter(|i| i.msg_type == xid_type && i.answers(xid, mac))
}

fn arp_lookup(my_mac: MacAddr, my_ip: Ipv4Addr, ip: Ipv4Addr) -> Option<MacAddr> {
    let req = ArpPacket { op: ArpOp::Request, sender_mac: my_mac, sender_ip: my_ip, target_mac: MacAddr::ZERO, target_ip: ip };
    let frame = EthernetHeader { dst: MacAddr::BROADCAST, src: my_mac, ethertype: EtherType::Arp }.build(&req.build());
    for _ in 0..3 {
        nic::send(&frame);
        let got = wait_frame(50, |rx| {
            let (h, p) = EthernetHeader::parse(rx).ok()?;
            if h.ethertype != EtherType::Arp {
                return None;
            }
            let a = ArpPacket::parse(p).ok()?;
            (a.op == ArpOp::Reply && a.sender_ip == ip).then_some(a.sender_mac)
        });
        if got.is_some() {
            return got;
        }
    }
    None
}

fn ip4(ip: Ipv4Addr) -> String {
    alloc::format!("{}.{}.{}.{}", ip.0[0], ip.0[1], ip.0[2], ip.0[3])
}

fn bring_up() {
    let Some(mac) = nic::mac() else { return };
    let my_mac = MacAddr(mac);
    let any = Ipv4Addr([0, 0, 0, 0]);
    let bcast = Ipv4Addr([255, 255, 255, 255]);
    let xid = crate::net::dhcp_xid();
    let send_dhcp = |mt: u8, req: Option<Ipv4Addr>, sid: Option<Ipv4Addr>| {
        let payload = dhcp::build(mt, xid, my_mac.0, req, sid);
        let seg = UdpDatagram { src_port: 68, dst_port: 67, payload }.build(any, bcast);
        let ipf = Ipv4Header { protocol: Protocol::Udp, ttl: 64, src: any, dst: bcast, total_length: 0, identification: 0x4e55 }.build(&seg);
        nic::send(&EthernetHeader { dst: MacAddr::BROADCAST, src: my_mac, ethertype: EtherType::Ipv4 }.build(&ipf));
    };

    // 1. DHCP. A machine without a DHCP server keeps asking, ever more slowly.
    let mut pause = 100u64;
    let mut tries = 0u32;
    let lease = loop {
        send_dhcp(dhcp::DISCOVER, None, None);
        if let Some(offer) = wait_frame(pause, |rx| dhcp_reply(rx, dhcp::OFFER, xid, my_mac.0)) {
            send_dhcp(dhcp::REQUEST, Some(offer.your_ip), Some(offer.server_id));
            // The ACK repeats the offer and may add options; without one the
            // offer stands (as on the inline path).
            let ack = wait_frame(200, |rx| dhcp_reply(rx, dhcp::ACK, xid, my_mac.0));
            let l = ack.unwrap_or(offer);
            break dhcp::DhcpInfo { router: l.router.or(offer.router), dns: l.dns.or(offer.dns), server_id: offer.server_id, ..l };
        }
        tries += 1;
        if tries == 1 {
            crate::serial_println!("[net-up] no DHCP offer in the first second over {}; asking again in the background", nic::kind());
        }
        pause = (pause * 2).min(1000);
    };
    let my_ip = lease.your_ip;
    let gw_ip = lease.router.unwrap_or(lease.server_id);
    let dns_ip = lease.dns.unwrap_or(gw_ip);

    // 2. ARP: the gateway, then the DNS server (the gateway forwards if it does
    // not answer itself).
    let gw_mac = arp_lookup(my_mac, my_ip, gw_ip);
    let dns_mac = if dns_ip == gw_ip { gw_mac } else { arp_lookup(my_mac, my_ip, dns_ip).or(gw_mac) };

    // 3. IPv6: link-local address, one router solicitation, 300 ms for the answer.
    let ll = Ipv6Addr::link_local_from_mac(my_mac.0);
    let rs = icmpv6::router_solicit(my_mac.0, ll, Ipv6Addr::ALL_ROUTERS);
    let h = Ipv6Header { next_header: 58, hop_limit: 255, src: ll, dst: Ipv6Addr::ALL_ROUTERS, payload_len: 0 };
    nic::send(&EthernetHeader { dst: MacAddr(Ipv6Addr::ALL_ROUTERS.multicast_mac()), src: my_mac, ethertype: EtherType::Ipv6 }.build(&h.build(&rs)));
    let router = wait_frame(30, |rx| {
        let (eh, p) = EthernetHeader::parse(rx).ok()?;
        if eh.ethertype != EtherType::Ipv6 {
            return None;
        }
        let (ih, pl) = Ipv6Header::parse(p).ok()?;
        (ih.next_header == 58 && icmpv6::msg_type(pl) == Some(icmpv6::ROUTER_ADVERT)).then_some((ih.src, eh.src))
    });

    // 4. Configured: from here on the receive routing owns the NIC.
    save(NetCfg {
        my_mac,
        my_ip,
        gw_ip,
        gw_mac: gw_mac.unwrap_or(MacAddr::ZERO),
        dns_ip,
        dns_mac: dns_mac.unwrap_or(MacAddr::ZERO),
        link_local: ll,
        router_ll: router.map(|r| r.0),
        router_mac: router.map(|r| r.1),
    });
    STATE.store(2, Ordering::Release);
    let took = crate::interrupts::ticks().saturating_sub(T_START.load(Ordering::Relaxed));
    crate::serial_println!(
        "[net-up] lease {} (gateway {}{}, dns {}, {} s) over {}; IPv6 router {}; {} ms after the task started{} ✓",
        ip4(my_ip),
        ip4(gw_ip),
        if gw_mac.is_some() { "" } else { " — no ARP answer" },
        ip4(dns_ip),
        lease.lease_secs,
        nic::kind(),
        if router.is_some() { "found" } else { "none" },
        took * 10,
        if tries > 0 { alloc::format!(" ({} unanswered DISCOVER)", tries) } else { String::new() }
    );
    worker::start();
    UP_EVENT.store(true, Ordering::Release);
}

extern "C" fn task_main() -> ! {
    bring_up();
    // A kernel task never returns: park. Nothing wakes this channel.
    loop {
        {
            let _g = crate::sched::IfOffGuard::new();
            crate::sched::block_on(CHAN);
        }
        crate::sched::yield_now();
    }
}
