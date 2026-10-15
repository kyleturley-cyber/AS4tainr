      * WRKCUST: work with customers. A load-all subfile (display file
      * CUSTWRKD): every customer is written to subfile CUSTSFL, the
      * control record CUSTCTL shows it a page at a time (Page Up/Down are
      * handled by the system), READC returns the lines the operator
      * typed an option on. Option 5 shows a customer. A wrong option is
      * marked with *IN50 (reverse image, SFLNXTCHG) and SFLMSG on *IN40.
      * F3 exits, F12 cancels.
     FCUSTWRKD  CF   E             WORKSTN
     F                                     SFILE(CUSTSFL:RRN)
     FCUSTMAST  IF   E           K DISK
     D RRN             S              4  0
      * Clear the subfile, then load every customer
     C                   EVAL      *IN32 = *ON
     C                   WRITE     CUSTCTL
     C                   EVAL      *IN32 = *OFF
     C                   EVAL      RRN = 0
     C                   READ      CUSTMAST
     C                   DOW       NOT %EOF
     C                   EVAL      RRN = RRN + 1
     C                   EVAL      OPT = ' '
     C                   WRITE     CUSTSFL
     C                   READ      CUSTMAST
     C                   ENDDO
     C                   IF        RRN > 0
     C                   EVAL      *IN31 = *ON
     C                   ENDIF
     C                   EVAL      *IN33 = *ON
      * Show the list until F3 or F12
     C                   DOW       NOT *IN03 AND NOT *IN12
     C                   WRITE     FOOTER
     C                   EXFMT     CUSTCTL
     C                   EVAL      *IN40 = *OFF
     C                   IF        NOT *IN03 AND NOT *IN12
     C                   READC     CUSTSFL
     C                   DOW       NOT %EOF
     C                   EVAL      *IN50 = *OFF
     C                   IF        OPT = '5'
     C     CUSNO         CHAIN     CUSTMAST
     C                   EXFMT     DETAIL
     C                   EVAL      *IN12 = *OFF
     C                   EVAL      OPT = ' '
     C                   ELSE
     C                   IF        OPT <> ' '
     C                   EVAL      *IN50 = *ON
     C                   EVAL      *IN40 = *ON
     C                   ENDIF
     C                   ENDIF
     C                   UPDATE    CUSTSFL
     C                   READC     CUSTSFL
     C                   ENDDO
     C                   ENDIF
     C                   ENDDO
     C                   SETON                                        LR
