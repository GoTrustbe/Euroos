/* Linux-style vDSO layout: ONE PT_LOAD (R+X) that contains the file header,
 * program headers, hashes, dynsym and text — the shape ld.so's setup_vdso was
 * written against. The clock data page is NOT part of the image at all: the
 * kernel maps it one page after (see __vdso_data in vdso.c). */
SECTIONS {
  . = SIZEOF_HEADERS;
  .hash           : { *(.hash) }            :text
  .gnu.hash       : { *(.gnu.hash) }
  .dynsym         : { *(.dynsym) }
  .dynstr         : { *(.dynstr) }
  .gnu.version    : { *(.gnu.version) }
  .gnu.version_d  : { *(.gnu.version_d) }
  .dynamic        : { *(.dynamic) }         :text :dynamic
  .rodata         : { *(.rodata*) }         :text
  .note           : { *(.note.*) }          :text
  .text           : { *(.text*) }           :text
  /DISCARD/       : { *(.data*) *(.bss*) *(.eh_frame*) *(.got*) *(.comment) }
}
PHDRS {
  text    PT_LOAD    FLAGS(5) FILEHDR PHDRS; /* R+X */
  dynamic PT_DYNAMIC FLAGS(4);
}
