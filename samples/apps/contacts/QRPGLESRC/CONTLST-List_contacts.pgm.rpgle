/// CONTLST: list the contacts in a full-load subfile, AS/400 style.
/// Documentation comments in column 1 are not code (columns 1-5 are the
/// sequence number area).
     FCONTLST   CF   E             WORKSTN SFILE(SFL01:RRN)
     FCONTACT   IF   E           K DISK
      /INCLUDE 'contacts.rpgleinc'
     C                   Z-ADD     0             RRN
     C                   SETON                                        30
     C                   WRITE     CTL01
     C                   SETOFF                                       30
     C     *LOVAL        SETLL     CONTACT
     C     *IN99         DOUEQ     *ON
     C                   READ      CONTACT                                99
     C     *IN99         IFEQ      *OFF
     C                   ADD       1             RRN
     C                   WRITE     SFL01
     C                   ENDIF
     C                   ENDDO
     C                   Z-ADD     RRN           COUNT
     C     RRN           IFGT      0
     C                   SETON                                        3132
     C                   ENDIF
     C                   WRITE     CMD01
     C     *INKC         DOUEQ     *ON
     C                   EXFMT     CTL01
     C                   ENDDO
     C                   SETON                                        LR
