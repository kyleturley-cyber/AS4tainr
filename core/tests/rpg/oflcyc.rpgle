      * Overflow in the cycle (ILE RPG Reference, "Overflow Routine",
      * "Detailed RPG IV Object Program Cycle"): printing on or past the
      * overflow line (FORMOFL 3) sets OF on; the next cycle prints the
      * lines conditioned by OF (the heading, skipping to line 1 of a
      * new page) before its detail line, once per overflow.
     FCUSTMAST  IP   E           K DISK
     FQSYSPRT   OF      132        PRINTER OFLIND(*INOF) FORMLEN(8) FORMOFL(3)
     OQSYSPRT   H    1P                     1  1
     O         OR    OF
     O                                           10 'HEADING'
     O          D   N1P                     1
     O                       CUSNO         Z      5
