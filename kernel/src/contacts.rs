//! Boot-zelftest voor **EuroContacts** (AC-3): vCard 3.0 + adresboek.
//! Kern: [`eurocontacts`].

use crate::serial_println;
use eurocontacts::AddressBook;

pub fn selftest() {
    let vcards = "BEGIN:VCARD\r\nVERSION:3.0\r\nN:Vandenberg;Jan;;;\r\nFN:Jan Vandenberg\r\nORG:EuroOS\r\nEMAIL;TYPE=work:jan@euro-os.eu\r\nTEL;TYPE=cell:+32470123456\r\nCATEGORIES:Kernteam\r\nEND:VCARD\r\nBEGIN:VCARD\r\nFN:Anna Bakker\r\nN:Bakker;Anna;;;\r\nEND:VCARD\r\n";
    let mut ab = AddressBook::from_vcards(vcards);
    let parsed = ab.contacts.len() == 2;
    ab.sort();
    let sorted = ab.contacts[0].family == "Bakker"; // Bakker < Vandenberg
    let jan = ab.contacts.iter().find(|c| c.full_name == "Jan Vandenberg");
    let fields_ok = jan.map(|c| c.org == "EuroOS" && c.primary_email() == Some("jan@euro-os.eu")).unwrap_or(false);
    let search_ok = ab.search("euro").len() == 1 && ab.in_group("Kernteam").len() == 1;
    // Round-trip: exporteren en opnieuw parsen behoudt het aantal.
    let roundtrip = AddressBook::from_vcards(&ab.export()).contacts.len() == 2;

    let ok = parsed && sorted && fields_ok && search_ok && roundtrip;
    serial_println!(
        "[ct] EuroContacts: vCards={}, sorteer(achternaam)={}, velden(ORG/EMAIL)={}, zoek+groep={}, round-trip={} {}",
        ab.contacts.len(), sorted, fields_ok, search_ok, roundtrip,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
