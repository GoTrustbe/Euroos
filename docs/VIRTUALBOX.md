# EuroOS in VirtualBox (Windows, macOS, Linux host)

EuroOS is a UEFI kernel with its own drivers; VirtualBox presents hardware
EuroOS already supports: UEFI firmware, a SATA (AHCI) disk, an Intel
PRO/1000 (e1000) or virtio network adapter, a VGA framebuffer through the
firmware's GOP, PS/2 and USB input. Measured on 2026-10-11 with QEMU
configured like VirtualBox (i440FX chipset, AHCI, Intel 82540EM, standard
VGA, 1 GB): the public image boots to the desktop, the NIC comes up and gets
a DHCP lease. Not yet booted inside VirtualBox itself; if you try it, the
serial log (below) tells us exactly what happened.

## Recipe

1. Download `euroos-x86_64-uefi.img.gz` from euro-os.eu/download, check the
   SHA256SUMS, unpack it (a raw 256 MiB disk image).
2. Convert it to a VirtualBox disk (Windows: in the VirtualBox folder):

       VBoxManage convertfromraw euroos-x86_64-uefi.img euroos.vdi --format VDI

3. New VM: type "Other", version "Other/Unknown (64-bit)", 1024 MB memory
   (512 MB works, 2048 MB is comfortable), 1 CPU is fine.
4. System: tick **Enable EFI (special OSes only)**; chipset **PIIX3**.
   Not ICH9 when Windows' hypervisor is active: VirtualBox then runs in its
   NEM mode, ICH9 adds a 64-bit PCI window at 896 GiB, the firmware programs
   a ninth variable MTRR (MSR 0x210) for it, that MSR is not virtualized and
   VirtualBox's own `CpuDxe.efi` dies with a #GP before EuroOS loads (seen
   2026-10-11). Pointing device: PS/2 mouse (USB input needs xHCI, which
   VirtualBox only has with the Extension Pack).
5. Storage: a **SATA** controller (AHCI) with `euroos.vdi` attached. Not IDE.
6. Network: adapter type **Intel PRO/1000 MT Desktop (82540EM)** or
   **Paravirtualized Network (virtio-net)**, attached to NAT.
7. Display: VBoxVGA or VMSVGA, 32 MB or more of video memory. 3D off.
8. Audio: Intel HD Audio if you want sound (EuroOS has an HDA driver).
9. Optional but useful: Serial Ports, port 1 enabled, mode "Raw File",
   path `C:\euroos-serial.log`. That is the kernel's log; send it along
   when something does not work.
10. Start. The lock screen appears; the live image has the demo accounts.

## What to expect and what not

- The live image runs from RAM; nothing is written to the virtual disk
  unless you run the installer from the wizard.
- VirtualBox has no KVM-style acceleration for a foreign kernel on Windows
  when Hyper-V or WSL2 is active; it then runs under the Hyper-V API and is
  slower. Turning off "Windows Hypervisor Platform" and Hyper-V makes
  VirtualBox use its own engine again.
- The Chromium pack and the model pack need a second disk: attach the
  `.img` as another SATA disk (raw, not converted). A pack must be attached
  as a data disk the kernel can read as a block device; VirtualBox's SATA
  is fine for that.
- Alternative with exactly our tested setup: QEMU for Windows with the
  Windows Hypervisor Platform:

       qemu-system-x86_64 -machine q35 -accel whpx -m 2048 -bios OVMF.fd ^
         -drive format=raw,file=euroos-x86_64-uefi.img ^
         -netdev user,id=n0 -device virtio-net-pci,netdev=n0 -vga std

## What a default "Other" VM gets wrong (seen in a real VBox.log, 2026-10-11)

VirtualBox's defaults for the "Other" OS type are an IDE (PIIX4) disk
controller, an AMD PCnet-FAST III network adapter, AC'97 audio and
OHCI/EHCI USB. EuroOS has none of those drivers (it has AHCI, NVMe, virtio
block; e1000 and virtio network; HD Audio; xHCI USB). The firmware still
loads the kernel from IDE and the live desktop boots from RAM, but there is
no disk for the installer, no network, and USB input needs xHCI (Extension
Pack only). Fix it from the command line with the VM powered off:

    cd "C:\Program Files\Oracle\VirtualBox"
    .\VBoxManage modifyvm "Euro-OS" --firmware efi --chipset piix3 --mouse ps2 --keyboard ps2 --audio-controller hda
    .\VBoxManage storagectl "Euro-OS" --name "IDE" --remove
    .\VBoxManage storagectl "Euro-OS" --name "SATA" --add sata --controller IntelAhci --portcount 2 --bootable on
    .\VBoxManage storageattach "Euro-OS" --storagectl "SATA" --port 0 --device 0 --type hdd --medium "C:\path\to\euroos.vmdk"
    .\VBoxManage modifyvm "Euro-OS" --nic1 nat --nictype1 82540EM
    .\VBoxManage modifyvm "Euro-OS" --uart1 0x3F8 4 --uartmode1 file "C:\euroos-serial.log"

`VBox.log` never contains the guest's own log; the serial file above does.
"HM: VT-x is not available, fall back to NEM" in VBox.log means Windows'
hypervisor (Hyper-V, WSL2, Windows Sandbox) is active and VirtualBox runs in
its slow "snail" mode; Hyper-V Generation 2 or QEMU with `-accel whpx` then
use that hypervisor directly instead.
