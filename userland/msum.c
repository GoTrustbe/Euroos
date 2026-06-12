/* EuroOS — een musl-programma dat NIET in de boot-set zit. Het wordt op naam
 * geïnstalleerd via de shell (`install msum`), waarbij de kernel zijn Ed25519-
 * handtekening verifieert vóór het in EuroFS te schrijven en te registreren.
 * Telt de gehele getallen uit argv op. */
#include <stdio.h>
#include <stdlib.h>

int main(int argc, char **argv) {
    long sum = 0;
    for (int i = 1; i < argc; i++) {
        sum += atol(argv[i]);
    }
    printf("som van %d getallen = %ld\n", argc - 1, sum);
    return 0;
}
