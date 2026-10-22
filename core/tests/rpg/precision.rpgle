**FREE
// The ILE RPG precision rules for numeric operations, as IBM i 7.5 applies
// them (PUB400, tools/golden/rpg-prec.txt): quotients have
// max(30 - ((L1 - D1) + D2), 0) decimal places, products D1 + D2, sums
// max(D1, D2); literals count their own digits.
dcl-s p72 packed(7: 2) inz(10);
dcl-s i10 int(10) inz(10);
dcl-s z92 zoned(9: 2) inz(1234.5);
dcl-s c char(50);
c = %char(10 / 3);
dsply c;
c = %char(100 / 3);
dsply c;
c = %char(p72 / 3);
dsply c;
c = %char(p72 / 0.3);
dsply c;
c = %char(i10 / 3);
dsply c;
c = %char(1 / 3 / 3);
dsply c;
c = %char(p72 * p72);
dsply c;
c = %char(2 / 3 * 3);
dsply c;
c = %char(z92 / 7);
dsply c;
c = %char(z92 * 0.001);
dsply c;
c = %char(-7 / 2);
dsply c;
*inlr = *on;
