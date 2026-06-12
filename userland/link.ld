/* Flat-binary layout voor EuroOS userspace: _start op offset 0, daarna code +
 * read-only data, alles aaneengesloten zodat objcopy er een platte (positie-
 * onafhankelijke) blob van maakt die de kernel op een willekeurig User-frame laadt. */
ENTRY(_start)
SECTIONS {
  . = 0;
  .text : {
    *(.text.start)   /* _start gegarandeerd eerst (offset 0) */
    *(.text*)
    *(.rodata*)
    *(.data*)
  }
  /* .bss in hetzelfde PT_LOAD-segment: filesz < memsz, kernel-loader nullt het. */
  .bss : { *(.bss*) *(COMMON) }
  /DISCARD/ : { *(.comment) *(.note*) *(.eh_frame*) }
}
