      * ORDENT: order entry (display file ORDENTD).
      *
      * CUSPRMT asks for a customer. ORDCTL shows the customer and an input
      * subfile ORDSFL of 20 lines (10 a page) for items and quantities.
      * Enter checks every line: the item must be in ITEMMAST and the
      * quantity more than zero; a wrong field is shown in reverse image
      * (*IN51, *IN52) with an SFLMSG. Prices, amounts and the order total
      * come from ITEMMAST. F6 creates the order: the number comes from the
      * data area ORDNXT (IN *LOCK / OUT), the header goes to ORDHDR, the
      * lines to ORDLIN, the quantities are taken off ITMQOH and the total
      * is added to the customer's balance. F12 goes back to the customer
      * prompt, F3 ends the program.
     FORDENTD   CF   E             WORKSTN
     F                                     SFILE(ORDSFL:RRN)
     FCUSTMAST  UF   E           K DISK
     FITEMMAST  UF   E           K DISK
     FORDHDR    O    E             DISK
     FORDLIN    O    E             DISK
     D SFLMAX          C                   20
     D RRN             S              4  0
     D LINES           S              3  0
     D ERRORS          S              1
     D DONE            S              1
     D NXTORD          S              7  0
     D YY              S              2  0
     D MM              S              2  0
     D DD              S              2  0
     C     *DTAARA       DEFINE    ORDNXT        NXTORD
      * Customer prompt until F3
     C                   DOW       NOT *IN03
     C                   EXFMT     CUSPRMT
     C                   EVAL      *IN60 = *OFF
     C                   IF        NOT *IN03
     C                   IF        CUSNUM = 0
     C                   EVAL      *IN35 = *ON
     C                   ELSE
     C     CUSNUM        CHAIN(N)  CUSTMAST
     C                   IF        %FOUND
     C                   EXSR      ENTER
     C                   ELSE
     C                   EVAL      *IN30 = *ON
     C                   ENDIF
     C                   ENDIF
     C                   ENDIF
     C                   ENDDO
     C                   SETON                                        LR
      *
      * ENTER: take the lines of one order
     C     ENTER         BEGSR
     C                   EVAL      *IN32 = *ON
     C                   WRITE     ORDCTL
     C                   EVAL      *IN32 = *OFF
     C                   EVAL      ITEM = *BLANKS
     C                   EVAL      QTY = 0
     C                   EVAL      ITMDSC = *BLANKS
     C                   EVAL      PRICE = 0
     C                   EVAL      AMOUNT = 0
     C                   EVAL      *IN51 = *OFF
     C                   EVAL      *IN52 = *OFF
     C                   EVAL      RRN = 1
     C                   DOW       RRN <= SFLMAX
     C                   WRITE     ORDSFL
     C                   EVAL      RRN = RRN + 1
     C                   ENDDO
     C                   EVAL      *IN31 = *ON
     C                   EVAL      *IN33 = *ON
     C                   EVAL      ORDTOT = 0
     C                   EVAL      DONE = 'N'
     C                   DOW       DONE = 'N'
     C                   WRITE     ORDFTR
     C                   EXFMT     ORDCTL
     C                   EVAL      *IN40 = *OFF
     C                   EVAL      *IN41 = *OFF
     C                   EVAL      *IN42 = *OFF
     C                   IF        *IN03 OR *IN12
     C                   EVAL      DONE = 'Y'
     C                   EVAL      *IN12 = *OFF
     C                   ELSE
     C                   EXSR      CHECK
     C                   IF        *IN06 AND ERRORS = 'N'
     C                   IF        LINES = 0
     C                   EVAL      *IN42 = *ON
     C                   ELSE
     C                   EXSR      WRTORD
     C                   EVAL      DONE = 'Y'
     C                   ENDIF
     C                   ENDIF
     C                   ENDIF
     C                   ENDDO
     C                   ENDSR
      *
      * CHECK: look up every line, mark errors, work out the total
     C     CHECK         BEGSR
     C                   EVAL      ERRORS = 'N'
     C                   EVAL      LINES = 0
     C                   EVAL      ORDTOT = 0
     C                   EVAL      RRN = 1
     C                   DOW       RRN <= SFLMAX
     C     RRN           CHAIN     ORDSFL
     C                   EVAL      *IN51 = *OFF
     C                   EVAL      *IN52 = *OFF
     C                   IF        ITEM = *BLANKS
     C                   EVAL      QTY = 0
     C                   EVAL      ITMDSC = *BLANKS
     C                   EVAL      PRICE = 0
     C                   EVAL      AMOUNT = 0
     C                   ELSE
     C     ITEM          CHAIN(N)  ITEMMAST
     C                   IF        %FOUND
     C                   EVAL      PRICE = ITMPRC
     C                   ELSE
     C                   EVAL      ITMDSC = *BLANKS
     C                   EVAL      PRICE = 0
     C                   EVAL      *IN51 = *ON
     C                   EVAL      *IN40 = *ON
     C                   EVAL      ERRORS = 'Y'
     C                   ENDIF
     C                   IF        QTY <= 0
     C                   EVAL      *IN52 = *ON
     C                   EVAL      *IN41 = *ON
     C                   EVAL      ERRORS = 'Y'
     C                   ENDIF
     C                   EVAL      AMOUNT = QTY * PRICE
     C                   EVAL      ORDTOT = ORDTOT + AMOUNT
     C                   EVAL      LINES = LINES + 1
     C                   ENDIF
     C                   UPDATE    ORDSFL
     C                   EVAL      RRN = RRN + 1
     C                   ENDDO
     C                   ENDSR
      *
      * WRTORD: create the order
     C     WRTORD        BEGSR
     C     *LOCK         IN        NXTORD
     C                   EVAL      ORDNO = NXTORD
     C                   EVAL      NXTORD = NXTORD + 1
     C                   OUT       NXTORD
     C                   Z-ADD     UYEAR         YY
     C                   Z-ADD     UMONTH        MM
     C                   Z-ADD     UDAY          DD
     C                   EVAL      ORDDAT = 20000000 + YY * 10000
     C                                    + MM * 100 + DD
     C                   EVAL      CUSNO = CUSNUM
     C                   EVAL      ORDLNS = LINES
     C                   EVAL      ORDSTS = 'O'
     C                   WRITE     ORDHDRR
     C                   EVAL      LINNO = 0
     C                   EVAL      RRN = 1
     C                   DOW       RRN <= SFLMAX
     C     RRN           CHAIN     ORDSFL
     C                   IF        ITEM <> *BLANKS
     C                   EVAL      LINNO = LINNO + 1
     C                   WRITE     ORDLINR
     C     ITEM          CHAIN     ITEMMAST
     C                   IF        %FOUND
     C                   EVAL      ITMQOH = ITMQOH - QTY
     C                   UPDATE    ITEMREC
     C                   ENDIF
     C                   ENDIF
     C                   EVAL      RRN = RRN + 1
     C                   ENDDO
     C     CUSNUM        CHAIN     CUSTMAST
     C                   IF        %FOUND
     C                   EVAL      CUSBAL = CUSBAL + ORDTOT
     C                   UPDATE    CUSTREC
     C                   ENDIF
     C                   EVAL      LSTORD = ORDNO
     C                   EVAL      LSTTOT = ORDTOT
     C                   EVAL      *IN60 = *ON
     C                   EVAL      CUSNUM = 0
     C                   ENDSR
