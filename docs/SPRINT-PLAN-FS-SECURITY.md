# Sprint: FS-beveiliging afgewerkt (rechten, immutability, integriteit, versioning)

Eigenaar (2026-08-28): bouw de 4 gaten uit het FS-onderzoek dicht, afgewerkt,
geen MVP. Volgorde = grootste beveiligingswinst eerst.

## Fase 1: rwx ECHT handhaven + chmod
Ontwerpbeslissingen (doordacht, niet impliciet):
- Handhaving in de FS-laag (disk.rs), tegen de sessie-uid-context:
  uid 0 = systeem/beheer = bypass (klassiek root-model).
  Eigenaar → owner-triple; iedereen anders → other-triple.
  Group-triple wordt OPGESLAGEN maar niet gehandhaafd: EuroOS heeft geen
  groepensysteem; dit staat gedocumenteerd (eerlijk, geen fake).
- Regels: read=r op bestand; overwrite=w op bestand; create/delete/rename=
  w op de PARENT-directory (POSIX-semantiek); list=r op de directory;
  chmod/chown = eigenaar of uid 0.
- chmod: nieuwe trait-methode + disk-implementatie + shell-commando +
  EuroFiles "Perms"-actie (octal-invoer, vooringevuld met huidige mode).
- KRITIEK, systeem mag zichzelf niet breken:
  a) Kernel-subsystemen die doorlopend systeembestanden schrijven (journal,
     audit, euroid-persist) draaien binnen een system-context-guard
     (uid tijdelijk 0, met save/restore). Dit is expliciet, geen sluiproute:
     het zijn kernel-diensten, geen user-acties.
  b) /home/<user>-migratie bij login: seeds zijn door uid 0 aangemaakt;
     bij sessie-open worden uid-0-bestanden onder /home/<user> ge-chownd
     naar de gebruiker (anders kan de gebruiker zijn eigen notes niet meer
     bewerken zodra rwx echt geldt).
  c) /etc/shadow en de users-db gaan naar 0600 (nu 0644 = wereld-leesbaar!).

## Fase 2: immutability zichtbaar + volledig
- Recursieve bescherming van /bin en /lib (alles immutable bij boot) +
  expliciete /etc-lijst (shadow, hostname; NIET de runtime-geschreven paden
  zoals /etc/eurousers, /etc/euroca, /etc/fde).
- EuroFiles: "protected"-badge uit de echte get_flags (nu alleen selftest);
  Protect/Unprotect-actie: eigen /home-bestanden via euroattr (user-weg),
  systeembestanden alleen met CAP_IMMUTABLE_ADMIN; nette weigering anders.
- FileOp-resultaten niet meer stil negeren: fout → statusmelding.
- euroattr shell-commando eindelijk wiren (bestond, hing los).

## Fase 3: systeem-integriteit op de live root
Eerlijk ontwerp: volle dm-verity vereist een read-only systeempartitie
(toekomst). Wat we NU afgewerkt bouwen: een Ed25519-GESIGNEERD manifest
(pad+SHA-256 van alles onder /bin,/lib) gegenereerd bij image-bouw;
de kernel verifieert bij boot en periodiek; mismatch → notificatie +
journal + shell-rapport `integrity`. Detecteert elke manipulatie van
systeembestanden, met bestaande signing-infra.

## Fase 4: per-bestand/directory versioning (op de CoW)
- Nieuwe inode-flag FLAG_VERSIONED (1<<2), zetbaar per bestand of directory
  (directory = geldt voor bestanden erin).
- Bij overwrite van een versioned bestand bewaart de FS de oude inhoud als
  versie-object onder /.versions/<pad-sleutel>/vN (gewone objecten: geen
  GC-gevaar), max 8 versies, ouder roteert weg.
- API: versions(path), restore_version(path,n); shell `versions`;
  EuroFiles "History"-actie: lijst + terugzetten.
- /.versions verborgen in EuroFiles-weergave.

## Verificatie per fase
Host-tests op eurofs (enforcement-matrix: owner/other × r/w/x-combinaties,
chmod-gating, versioning-rotatie), boot-selftests, QMP-screenshotproofs
(Perms-dialoog, protected-badge, History-restore), volle suite groen,
sanity-boot met default-NIC vóór publicatie.
