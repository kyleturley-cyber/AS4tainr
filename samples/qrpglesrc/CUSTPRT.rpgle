      * CUSTPRT: customer balance report on printer file QSYSPRT.
      *
      * A program-described printer file with O specs: EXCEPT HEADING at
      * the start and whenever the overflow indicator OF comes on (line 60
      * of 66), EXCEPT DETAIL per customer (a * after a negative balance),
      * EXCEPT TOTALS at the end. The report becomes spooled file QSYSPRT
      * of the job (WRKSPLF); `as4tainr call` prints it to standard output.
     FCUSTMAST  IF   E           K DISK
     FQSYSPRT   O    F  132        PRINTER OFLIND(*INOF)
     D TOTAL           S             11  2
     D COUNT           S              5  0
     C                   EXCEPT    HEADING
     C                   READ      CUSTMAST
     C                   DOW       NOT %EOF
     C                   IF        *INOF
     C                   EXCEPT    HEADING
     C                   EVAL      *INOF = *OFF
     C                   ENDIF
     C                   IF        CUSBAL < 0
     C                   EVAL      *IN50 = *ON
     C                   ELSE
     C                   EVAL      *IN50 = *OFF
     C                   ENDIF
     C                   EXCEPT    DETAIL
     C                   EVAL      TOTAL = TOTAL + CUSBAL
     C                   EVAL      COUNT = COUNT + 1
     C                   READ      CUSTMAST
     C                   ENDDO
     C                   EXCEPT    TOTALS
     C                   SETON                                        LR
     OQSYSPRT   E            HEADING        2  1
     O                       UDATE         Y      8
     O                                           50 'Customer Balance Report'
     O                                           78 'Page'
     O                       PAGE          Z     83
     O          E            HEADING        1
     O                                            7 'Number'
     O                                           15 'Name'
     O                                           48 'City'
     O                                           82 'Balance'
     O          E            DETAIL         1
     O                       CUSNO         Z      7
     O                       CUSNAM              41
     O                       CUSCTY              64
     O                       CUSBAL        J     83
     O               50                          84 '*'
     O          E            TOTALS      1  1
     O                                           19 'Customers:'
     O                       COUNT         Z     25
     O                                           64 'Total balance:'
     O                       TOTAL         J     83
