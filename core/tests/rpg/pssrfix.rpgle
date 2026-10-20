      * *PSSR (ILE RPG Reference, "Program Exception/Error Subroutine"):
      * a divide by zero (status 00102) in the detail calculations
      * (*ROUTINE *DETC) goes to *PSSR; GOTO from it back retries.
     D                SDS
     DSTAT               *STATUS
     DRTN                *ROUTINE
     DZ                S              5P 0 INZ(0)
     DN                S              5P 0
     C     RETRY         TAG
     C                   EVAL      N = 10 / Z
     C     N             DSPLY
     C                   SETON                                          LR
     C     *PSSR         BEGSR
     C     STAT          DSPLY
     C     RTN           DSPLY
     C                   EVAL      Z = 2
     C                   GOTO      RETRY
     C                   ENDSR
