/* Database: SQLite in write-ahead-log mode on the Nova volume (phase 12).
 *
 * The browser keeps its profile in SQLite databases, most of them in WAL
 * mode: the log beside the database, and a -shm index that every
 * connection maps shared and writable. This test does what a profile
 * database does -- create, switch to WAL, write, read back through a
 * second connection -- in a directory of the volume (/etc is one), then
 * removes its files. Before shared mappings of volume files existed, the
 * index could not be mapped and SQLite answered SQLITE_IOERR_SHMMAP. */
#include <stdio.h>
#include <string.h>
#include <unistd.h>
#include <sqlite3.h>

#define PATH "/etc/nova-base.sqlite"

static int exec(sqlite3 *db, const char *sql) {
    char *err = 0;
    int rc = sqlite3_exec(db, sql, 0, 0, &err);
    if (rc != SQLITE_OK) {
        printf("database: \"%s\" -> %d (etendu %d) %s\n", sql, rc, sqlite3_extended_errcode(db), err ? err : "");
        sqlite3_free(err);
    }
    return rc;
}

static int count_rows(sqlite3 *db) {
    sqlite3_stmt *st;
    int n = -1;
    if (sqlite3_prepare_v2(db, "SELECT count(*) FROM t", -1, &st, 0) != SQLITE_OK) return -1;
    if (sqlite3_step(st) == SQLITE_ROW) n = sqlite3_column_int(st, 0);
    sqlite3_finalize(st);
    return n;
}

int main(void) {
    unlink(PATH);
    unlink(PATH "-wal");
    unlink(PATH "-shm");

    sqlite3 *a, *b;
    if (sqlite3_open_v2(PATH, &a, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, 0) != SQLITE_OK) return 1;
    if (exec(a, "PRAGMA journal_mode=WAL")) return 2;
    if (exec(a, "CREATE TABLE t(a INTEGER, b TEXT)")) return 3;
    if (exec(a, "BEGIN")) return 4;
    for (int i = 0; i < 200; i++) {
        char sql[96];
        snprintf(sql, sizeof sql, "INSERT INTO t VALUES(%d, 'ligne %d')", i, i);
        if (exec(a, sql)) return 5;
    }
    if (exec(a, "COMMIT")) return 6;
    if (access(PATH "-shm", F_OK) != 0) return 7;

    /* A second connection reads what the first committed: it finds the
     * commit through the shared index, the log not yet checkpointed. */
    if (sqlite3_open_v2(PATH, &b, SQLITE_OPEN_READWRITE, 0) != SQLITE_OK) return 8;
    if (count_rows(b) != 200) return 9;
    if (exec(b, "INSERT INTO t VALUES(200, 'seconde connexion')")) return 10;
    if (count_rows(a) != 201) return 11;
    if (exec(a, "PRAGMA wal_checkpoint(TRUNCATE)")) return 12;
    sqlite3_close(b);
    sqlite3_close(a);

    /* Reopened from the disk: the rows are in the database file. */
    if (sqlite3_open_v2(PATH, &a, SQLITE_OPEN_READONLY, 0) != SQLITE_OK) return 13;
    int rows = count_rows(a);
    sqlite3_close(a);
    if (rows != 201) return 14;

    unlink(PATH);
    unlink(PATH "-wal");
    unlink(PATH "-shm");
    printf("database: SQLite %s en WAL sur le volume Nova, %d lignes, deux connexions conformes\n", sqlite3_libversion(), rows);
    return 0;
}
