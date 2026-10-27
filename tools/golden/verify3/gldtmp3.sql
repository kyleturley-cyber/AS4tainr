-- Golden: the verify2 temporal table without the COMMIT that SQL7007
-- refused, so that the script completes (AS4TRAINR1.GLDTMP, GLDTMP_HST).
set schema = AS4TRAINR1;
create table GLDTMP (
    GRP        char(10) not null,
    SEQ        int not null,
    CMD        varchar(100) not null,
    AUDIT_USER varchar(128) generated always as (SESSION_USER),
    AUDIT_OPER char(1) generated always as (data change operation),
    SYS_START  timestamp(12) not null generated always as row begin,
    SYS_END    timestamp(12) not null generated always as row end,
    TS_ID      timestamp(12) not null generated always as transaction start id,
    period system_time (SYS_START, SYS_END)
);
alter table GLDTMP add primary key (GRP, SEQ);
create table GLDTMP_HST like GLDTMP;
alter table GLDTMP add versioning use history table GLDTMP_HST on delete add extra row;
alter table GLDTMP
    alter column SYS_START set implicitly hidden
    alter column SYS_END set implicitly hidden
    alter column TS_ID set implicitly hidden;
insert into GLDTMP (GRP, SEQ, CMD) values ('G1', 1, 'DSPLIBL');
