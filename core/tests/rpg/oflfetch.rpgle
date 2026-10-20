      * Fetch overflow (ILE RPG Reference, "Fetch Overflow Routine"; F in
      * position 18): when OF is on, the overflow lines print before the
      * line that fetches them, not at the end of the cycle.
     FQSYSPRT   OF      132        PRINTER OFLIND(*INOF) FORMLEN(8) FORMOFL(3)
     DI                S              3P 0
     C                   DO        5             I
     C                   EXCEPT    LINE
     C                   ENDDO
     C                   SETON                                          LR
     OQSYSPRT   H    OF                     1  1
     O                                           10 'HEADING'
     O          EF           LINE           1
     O                       I             Z      5
