     H DFTACTGRP(*NO)
      * Golden listing of fixed-form RPG IV (tools/golden/listings2.txt).
     FGLDLSTF   IF   E             DISK
     D Total           S              9P 2
     D Count           S              5I 0
     C                   READ      GLDLSTR                                90
     C                   DOW       *IN90 = *OFF
     C                   ADD       AMT           Total
     C                   EVAL      Count = Count + 1
     C                   IF        Count > 100
     C                   LEAVE
     C                   ENDIF
     C                   READ      GLDLSTR                                90
     C                   ENDDO
     C     Total         DSPLY
     C                   SETON                                        LR
