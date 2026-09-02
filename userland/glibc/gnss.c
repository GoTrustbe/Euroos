#include <stdio.h>
#include <nss.h>
#include <nspr.h>
#include <pk11pub.h>
#include <secerr.h>

/* Can NSS initialise here? Chrome verifies every server certificate through it,
   and when it cannot start, the TLS handshake stops right after the server's
   first flight: the page loads to ready=complete with an empty document, and
   nothing in the browser says why.

   Chrome reports NSS error -8023 = SEC_ERROR_PKCS11_DEVICE_ERROR, which comes
   from the software token. This runs the same initialisation in one small
   program, so the answer takes a boot instead of a browser run.

   Exit 167 = NSS initialised and its internal token answered. */
int main(void) {
    SECStatus rv = NSS_NoDB_Init(".");
    if (rv != SECSuccess) {
        printf("GNSS: NSS_NoDB_Init failed, error %d\n", (int)PR_GetError());
        fflush(stdout);
        return 2;
    }
    printf("GNSS: NSS_NoDB_Init OK\n");
    PK11SlotInfo *slot = PK11_GetInternalSlot();
    if (!slot) {
        printf("GNSS: no internal slot, error %d\n", (int)PR_GetError());
        fflush(stdout);
        return 3;
    }
    unsigned char buf[32];
    if (PK11_GenerateRandom(buf, sizeof buf) != SECSuccess) {
        printf("GNSS: PK11_GenerateRandom failed, error %d\n", (int)PR_GetError());
        fflush(stdout);
        return 4;
    }
    printf("GNSS: internal token works, random[0..3]=%02x%02x%02x%02x\n",
           buf[0], buf[1], buf[2], buf[3]);
    fflush(stdout);
    return 167;
}
