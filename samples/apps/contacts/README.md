# contacts: a small legacy-style application

A sample used to test `as4tainr import`: AS/400-style fixed-form RPG with a
full-load subfile, a CL program with INCLUDE, DDS with comment lines, and
CSV data, laid out the way IBM i source is kept in git (TOBi naming:
`OBJECT-Description.pgm.rpgle`). Written for as4tainr.

    as4tainr import samples/apps/contacts --lib CONTACTS
    CALL CONTACTS/CONTACTS        (on a 5250 session)
    as4tainr --libl CONTACTS call CONTACTS/CONTCNT
