      * The cycle with *PSSR (ILE RPG Reference, "File Exception/Error
      * Subroutine (INFSR)", return points): ENDSR *GETIN goes on with the
      * next record; the return point field is blank after ENDSR.
     FCUSTMAST  IP   E           K DISK
     D                SDS
     DSTAT               *STATUS
     DRP               S              6    INZ('*GETIN')
     DZ                S              5P 0
     C                   IF        CUSBAL < 0
     C                   EVAL      Z = 1 / Z
     C                   ENDIF
     C     CUSNO         DSPLY
     C     *PSSR         BEGSR
     C     'skip'        DSPLY
     C     RP            DSPLY
     C                   ENDSR     RP
