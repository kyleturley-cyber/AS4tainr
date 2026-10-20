      * INFSR (ILE RPG Reference, "File Exception/Error Subroutine"): a
      * WRITE of a duplicate key (status 01021) without an error indicator
      * goes to the file's INFSR subroutine.
     FCUSTMAST  UF A E           K DISK    INFSR(FILERR) INFDS(FB)
     DFB               DS
     DFBSTAT             *STATUS
     DFBOP               *OPCODE
     DDONE             S              1    INZ('N')
     C                   EVAL      CUSNO = 1
     C                   EVAL      CUSNAM = 'Again'
     C                   WRITE     CUSTREC
     C     BACK          TAG
     C     DONE          DSPLY
     C                   SETON                                          LR
     C     FILERR        BEGSR
     C     FBSTAT        DSPLY
     C                   EVAL      DONE = 'Y'
     C                   GOTO      BACK
     C                   ENDSR
