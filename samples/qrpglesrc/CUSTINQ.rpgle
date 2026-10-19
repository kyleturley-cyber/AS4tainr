      * CUSTINQ: interactive customer inquiry (display file CUSTINQD).
      * PROMPT asks for a customer number; DETAIL shows the customer.
      * F3 ends the program; F12 on DETAIL returns to PROMPT.
      * An unknown customer turns on *IN30, which shows ERRMSG on PROMPT.
     FCUSTINQD  CF   E             WORKSTN
     FCUSTMAST  IF   E           K DISK
     C                   DOW       NOT *IN03
     C                   EXFMT     PROMPT
     C                   IF        NOT *IN03
     C     CUSNUM        CHAIN     CUSTMAST
     C                   IF        %FOUND
     C                   IF        CUSBAL < 0
     C                   EVAL      *IN40 = *ON
     C                   ELSE
     C                   EVAL      *IN40 = *OFF
     C                   ENDIF
     C                   EXFMT     DETAIL
     C                   ELSE
     C                   EVAL      *IN30 = *ON
     C                   ENDIF
     C                   ENDIF
     C                   ENDDO
     C                   SETON                                        LR
