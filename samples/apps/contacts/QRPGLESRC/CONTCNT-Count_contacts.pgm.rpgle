/// CONTCNT: count the contacts, then look up three numbers.
     FCONTACT   IF   E           K DISK
      /INCLUDE 'contacts.rpgleinc'
     D KEY             S              5  0
     C     *LOVAL        SETLL     CONTACT
     C     *IN99         DOUEQ     *ON
     C                   READ      CONTACT                                99
     C     *IN99         IFEQ      *OFF
     C                   ADD       1             RRN
     C                   ENDIF
     C                   ENDDO
     C     RRN           DSPLY
      * CHAIN: the indicator in positions 71-72 is on when no record is found.
     C                   Z-ADD     2             KEY
     C                   EXSR      LOOKUP
     C                   Z-ADD     9             KEY
     C                   EXSR      LOOKUP
     C                   Z-ADD     4             KEY
     C                   EXSR      LOOKUP
     C     FOUND         DSPLY
     C     MISSING       DSPLY
     C                   SETON                                        LR
     C     LOOKUP        BEGSR
     C     KEY           CHAIN     CONTACT                            50
     C     *IN50         IFEQ      *ON
     C                   ADD       1             MISSING
     C                   ELSE
     C                   ADD       1             FOUND
     C     CTNAME        DSPLY
     C                   ENDIF
     C                   ENDSR
