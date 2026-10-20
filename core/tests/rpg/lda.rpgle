      * A data area data structure without a name is the local data area
      * (ILE RPG Reference, U in position 23): read when the program
      * starts, written when it ends with LR.
     D                UDS
     DLDAKEY                   1      5
     DLDAAMT                   6     10P 2
     C     LDAKEY        DSPLY
     C     LDAAMT        DSPLY
     C                   EVAL      LDAKEY = 'NEXT'
     C                   EVAL      LDAAMT = LDAAMT + 1.25
     C                   SETON                                          LR
