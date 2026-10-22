**FREE
// %EDITC's third argument (ILE RPG Reference, %EDITC): *ASTFILL fills the
// leading blanks with asterisks; *CURSYM or a currency literal floats left
// of the first digit, one position more. %CHAR with a date format and its
// separator (*MDY-).
dcl-s pos packed(9: 2) inz(1234.56);
dcl-s d date inz(d'2026-10-22');
dcl-s c char(20);
c = %editc(pos: 'J': *astfill);
dsply c;
c = %editc(pos: 'J': *cursym);
dsply c;
c = %editc(pos: '1': 'X');
dsply c;
c = %char(d: *mdy-);
dsply c;
*inlr = *on;
