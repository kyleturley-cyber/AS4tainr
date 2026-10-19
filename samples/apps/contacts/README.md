# contacts: a small legacy-style application

A sample used to test `ibmish import`: AS/400-style fixed-form RPG with a
full-load subfile, a CL program with INCLUDE, DDS with comment lines, and
CSV data, laid out the way IBM i source is kept in git (TOBi naming:
`OBJECT-Description.pgm.rpgle`). Written for ibmish.

    ibmish import samples/apps/contacts --lib CONTACTS
    CALL CONTACTS/CONTACTS        (on a 5250 session)
    ibmish --libl CONTACTS call CONTACTS/CONTCNT
