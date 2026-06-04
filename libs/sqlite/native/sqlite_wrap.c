#include "sqlite_wrap.h"

#include "ciel_core.h"
#include "ciel_gc.h"
#include "sqlite3.h"

#include <limits.h>

struct CielSqliteConnection {
    sqlite3* db;
};

struct CielSqliteStatement {
    sqlite3_stmt* stmt;
};

static int slice_len_to_int(size_t len, int* out) {
    if (out == NULL)
        return SQLITE_MISUSE;
    if (len > (size_t)INT_MAX)
        return SQLITE_TOOBIG;
    *out = (int)len;
    return SQLITE_OK;
}

static int bind_index(size_t index, int* out) {
    if (out == NULL)
        return SQLITE_MISUSE;
    if (index >= (size_t)INT_MAX)
        return SQLITE_RANGE;
    *out = (int)index + 1;
    return SQLITE_OK;
}

static int column_index(CielSqliteStatement* statement, size_t index, int* out) {
    if (statement == NULL || statement->stmt == NULL || out == NULL)
        return SQLITE_MISUSE;
    if (index > (size_t)INT_MAX)
        return SQLITE_RANGE;
    int column_count = sqlite3_column_count(statement->stmt);
    if (index >= (size_t)column_count)
        return SQLITE_RANGE;
    *out = (int)index;
    return SQLITE_OK;
}

static int valid_connection(CielSqliteConnection* connection) {
    return connection != NULL && connection->db != NULL;
}

static int valid_statement(CielSqliteStatement* statement) {
    return statement != NULL && statement->stmt != NULL;
}

static void connection_finalizer(void* obj, void* client_data) {
    (void)client_data;
    CielSqliteConnection* connection = (CielSqliteConnection*)obj;
    if (connection != NULL && connection->db != NULL) {
        sqlite3_close_v2(connection->db);
        connection->db = NULL;
    }
}

static void statement_finalizer(void* obj, void* client_data) {
    (void)client_data;
    CielSqliteStatement* statement = (CielSqliteStatement*)obj;
    if (statement != NULL && statement->stmt != NULL) {
        sqlite3_finalize(statement->stmt);
        statement->stmt = NULL;
    }
}

static int sqlite_open_flags(int mode) {
    switch (mode) {
    case 1:
        return SQLITE_OPEN_READONLY | SQLITE_OPEN_URI;
    case 2:
        return SQLITE_OPEN_READWRITE | SQLITE_OPEN_URI;
    default:
        return SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE | SQLITE_OPEN_URI;
    }
}

static int wrap_open_cstr(const char* path, int flags,
                          CielSqliteConnection** out) {
    if (path == NULL || out == NULL)
        return SQLITE_MISUSE;
    *out = NULL;
    sqlite3* db = NULL;
    int rc = sqlite3_open_v2(path, &db, flags, NULL);
    if (rc != SQLITE_OK) {
        if (db != NULL)
            sqlite3_close_v2(db);
        return rc;
    }
    CielSqliteConnection* connection =
        (CielSqliteConnection*)ciel_alloc(sizeof(CielSqliteConnection));
    connection->db = db;
    ciel_register_finalizer(connection, connection_finalizer, NULL);
    *out = connection;
    return SQLITE_OK;
}

int ciel_sqlite_open(const char* path, size_t path_len, int flags,
                     CielSqliteConnection** out) {
    if (path == NULL && path_len != 0)
        return SQLITE_MISUSE;
    char* c_path = ciel_cstr_from_slice(path == NULL ? "" : path, path_len);
    return wrap_open_cstr(c_path, sqlite_open_flags(flags), out);
}

int ciel_sqlite_open_memory(CielSqliteConnection** out) {
    return wrap_open_cstr(":memory:",
                          SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE |
                              SQLITE_OPEN_MEMORY,
                          out);
}

int ciel_sqlite_close(CielSqliteConnection* connection) {
    if (connection == NULL)
        return SQLITE_MISUSE;
    if (connection->db == NULL)
        return SQLITE_OK;
    sqlite3* db = connection->db;
    connection->db = NULL;
    return sqlite3_close_v2(db);
}

int ciel_sqlite_exec(CielSqliteConnection* connection, const char* sql,
                     size_t sql_len) {
    if (!valid_connection(connection) || (sql == NULL && sql_len != 0))
        return SQLITE_MISUSE;
    char* c_sql = ciel_cstr_from_slice(sql == NULL ? "" : sql, sql_len);
    char* error = NULL;
    int rc = sqlite3_exec(connection->db, c_sql, NULL, NULL, &error);
    if (error != NULL)
        sqlite3_free(error);
    return rc;
}

int ciel_sqlite_busy_timeout(CielSqliteConnection* connection, int milliseconds) {
    if (!valid_connection(connection))
        return SQLITE_MISUSE;
    return sqlite3_busy_timeout(connection->db, milliseconds);
}

void ciel_sqlite_interrupt(CielSqliteConnection* connection) {
    if (valid_connection(connection))
        sqlite3_interrupt(connection->db);
}

int ciel_sqlite_last_insert_rowid(CielSqliteConnection* connection,
                                  int64_t* out) {
    if (!valid_connection(connection) || out == NULL)
        return SQLITE_MISUSE;
    *out = (int64_t)sqlite3_last_insert_rowid(connection->db);
    return SQLITE_OK;
}

int ciel_sqlite_changes(CielSqliteConnection* connection, int64_t* out) {
    if (!valid_connection(connection) || out == NULL)
        return SQLITE_MISUSE;
    *out = (int64_t)sqlite3_changes64(connection->db);
    return SQLITE_OK;
}

int ciel_sqlite_total_changes(CielSqliteConnection* connection, int64_t* out) {
    if (!valid_connection(connection) || out == NULL)
        return SQLITE_MISUSE;
    *out = (int64_t)sqlite3_total_changes64(connection->db);
    return SQLITE_OK;
}

int ciel_sqlite_prepare(CielSqliteConnection* connection, const char* sql,
                        size_t sql_len, CielSqliteStatement** out) {
    if (!valid_connection(connection) || out == NULL ||
        (sql == NULL && sql_len != 0))
        return SQLITE_MISUSE;
    *out = NULL;
    int nbyte = 0;
    int rc = slice_len_to_int(sql_len, &nbyte);
    if (rc != SQLITE_OK)
        return rc;
    sqlite3_stmt* stmt = NULL;
    rc = sqlite3_prepare_v2(connection->db, sql == NULL ? "" : sql, nbyte,
                            &stmt, NULL);
    if (rc != SQLITE_OK)
        return rc;
    CielSqliteStatement* statement =
        (CielSqliteStatement*)ciel_alloc(sizeof(CielSqliteStatement));
    statement->stmt = stmt;
    ciel_register_finalizer(statement, statement_finalizer, NULL);
    *out = statement;
    return SQLITE_OK;
}

int ciel_sqlite_finalize(CielSqliteStatement* statement) {
    if (statement == NULL)
        return SQLITE_MISUSE;
    if (statement->stmt == NULL)
        return SQLITE_OK;
    sqlite3_stmt* stmt = statement->stmt;
    statement->stmt = NULL;
    return sqlite3_finalize(stmt);
}

int ciel_sqlite_reset(CielSqliteStatement* statement) {
    if (!valid_statement(statement))
        return SQLITE_MISUSE;
    return sqlite3_reset(statement->stmt);
}

int ciel_sqlite_clear_bindings(CielSqliteStatement* statement) {
    if (!valid_statement(statement))
        return SQLITE_MISUSE;
    return sqlite3_clear_bindings(statement->stmt);
}

int ciel_sqlite_step(CielSqliteStatement* statement) {
    if (!valid_statement(statement))
        return SQLITE_MISUSE;
    return sqlite3_step(statement->stmt);
}

int ciel_sqlite_bind_parameter_count(CielSqliteStatement* statement,
                                     size_t* out) {
    if (!valid_statement(statement) || out == NULL)
        return SQLITE_MISUSE;
    int count = sqlite3_bind_parameter_count(statement->stmt);
    *out = count < 0 ? 0 : (size_t)count;
    return SQLITE_OK;
}

int ciel_sqlite_bind_parameter_index(CielSqliteStatement* statement,
                                     const char* name, size_t name_len,
                                     size_t* out) {
    if (!valid_statement(statement) || out == NULL ||
        (name == NULL && name_len != 0))
        return SQLITE_MISUSE;
    char* c_name = ciel_cstr_from_slice(name == NULL ? "" : name, name_len);
    int index = sqlite3_bind_parameter_index(statement->stmt, c_name);
    if (index <= 0)
        return SQLITE_RANGE;
    *out = (size_t)(index - 1);
    return SQLITE_OK;
}

int ciel_sqlite_bind_null(CielSqliteStatement* statement, size_t index) {
    if (!valid_statement(statement))
        return SQLITE_MISUSE;
    int sqlite_index = 0;
    int rc = bind_index(index, &sqlite_index);
    if (rc != SQLITE_OK)
        return rc;
    return sqlite3_bind_null(statement->stmt, sqlite_index);
}

int ciel_sqlite_bind_i64(CielSqliteStatement* statement, size_t index,
                         int64_t value) {
    if (!valid_statement(statement))
        return SQLITE_MISUSE;
    int sqlite_index = 0;
    int rc = bind_index(index, &sqlite_index);
    if (rc != SQLITE_OK)
        return rc;
    return sqlite3_bind_int64(statement->stmt, sqlite_index,
                              (sqlite3_int64)value);
}

int ciel_sqlite_bind_f64(CielSqliteStatement* statement, size_t index,
                         double value) {
    if (!valid_statement(statement))
        return SQLITE_MISUSE;
    int sqlite_index = 0;
    int rc = bind_index(index, &sqlite_index);
    if (rc != SQLITE_OK)
        return rc;
    return sqlite3_bind_double(statement->stmt, sqlite_index, value);
}

int ciel_sqlite_bind_text(CielSqliteStatement* statement, size_t index,
                          const char* text, size_t text_len) {
    if (!valid_statement(statement) || (text == NULL && text_len != 0))
        return SQLITE_MISUSE;
    int sqlite_index = 0;
    int rc = bind_index(index, &sqlite_index);
    if (rc != SQLITE_OK)
        return rc;
    return sqlite3_bind_text64(statement->stmt, sqlite_index,
                               text == NULL ? "" : text,
                               (sqlite3_uint64)text_len, SQLITE_TRANSIENT,
                               SQLITE_UTF8);
}

int ciel_sqlite_bind_blob(CielSqliteStatement* statement, size_t index,
                          const uint8_t* data, size_t data_len) {
    if (!valid_statement(statement) || (data == NULL && data_len != 0))
        return SQLITE_MISUSE;
    int sqlite_index = 0;
    int rc = bind_index(index, &sqlite_index);
    if (rc != SQLITE_OK)
        return rc;
    return sqlite3_bind_blob64(statement->stmt, sqlite_index,
                               data_len == 0 ? "" : (const void*)data,
                               (sqlite3_uint64)data_len, SQLITE_TRANSIENT);
}

int ciel_sqlite_column_count(CielSqliteStatement* statement, size_t* out) {
    if (!valid_statement(statement) || out == NULL)
        return SQLITE_MISUSE;
    int count = sqlite3_column_count(statement->stmt);
    *out = count < 0 ? 0 : (size_t)count;
    return SQLITE_OK;
}

int ciel_sqlite_column_type(CielSqliteStatement* statement, size_t index,
                            int* out) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if (out == NULL)
        return SQLITE_MISUSE;
    *out = sqlite3_column_type(statement->stmt, column);
    return SQLITE_OK;
}

int ciel_sqlite_column_i64(CielSqliteStatement* statement, size_t index,
                           int64_t* out) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if (out == NULL)
        return SQLITE_MISUSE;
    *out = (int64_t)sqlite3_column_int64(statement->stmt, column);
    return SQLITE_OK;
}

int ciel_sqlite_column_f64(CielSqliteStatement* statement, size_t index,
                           double* out) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if (out == NULL)
        return SQLITE_MISUSE;
    *out = sqlite3_column_double(statement->stmt, column);
    return SQLITE_OK;
}

int ciel_sqlite_column_name_len(CielSqliteStatement* statement, size_t index,
                                size_t* out) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if (out == NULL)
        return SQLITE_MISUSE;
    const char* name = sqlite3_column_name(statement->stmt, column);
    if (name == NULL)
        return SQLITE_NOMEM;
    *out = strlen(name);
    return SQLITE_OK;
}

int ciel_sqlite_column_name_copy_to(CielSqliteStatement* statement,
                                    size_t index, char* out, size_t cap,
                                    size_t* copied) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if ((out == NULL && cap != 0) || copied == NULL)
        return SQLITE_MISUSE;
    const char* name = sqlite3_column_name(statement->stmt, column);
    if (name == NULL)
        return SQLITE_NOMEM;
    size_t needed = strlen(name);
    size_t n = needed < cap ? needed : cap;
    if (n != 0)
        memcpy(out, name, n);
    *copied = n;
    return SQLITE_OK;
}

int ciel_sqlite_column_text_len(CielSqliteStatement* statement, size_t index,
                                size_t* out) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if (out == NULL)
        return SQLITE_MISUSE;
    const unsigned char* text = sqlite3_column_text(statement->stmt, column);
    int bytes = sqlite3_column_bytes(statement->stmt, column);
    if (text == NULL && bytes > 0)
        return SQLITE_NOMEM;
    *out = bytes < 0 ? 0 : (size_t)bytes;
    return SQLITE_OK;
}

int ciel_sqlite_column_text_copy_to(CielSqliteStatement* statement,
                                    size_t index, char* out, size_t cap,
                                    size_t* copied) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if ((out == NULL && cap != 0) || copied == NULL)
        return SQLITE_MISUSE;
    const unsigned char* text = sqlite3_column_text(statement->stmt, column);
    int bytes = sqlite3_column_bytes(statement->stmt, column);
    if (text == NULL && bytes > 0)
        return SQLITE_NOMEM;
    size_t needed = bytes < 0 ? 0 : (size_t)bytes;
    size_t n = needed < cap ? needed : cap;
    if (n != 0)
        memcpy(out, text, n);
    *copied = n;
    return SQLITE_OK;
}

int ciel_sqlite_column_blob_len(CielSqliteStatement* statement, size_t index,
                                size_t* out) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if (out == NULL)
        return SQLITE_MISUSE;
    (void)sqlite3_column_blob(statement->stmt, column);
    int bytes = sqlite3_column_bytes(statement->stmt, column);
    *out = bytes < 0 ? 0 : (size_t)bytes;
    return SQLITE_OK;
}

int ciel_sqlite_column_blob_copy_to(CielSqliteStatement* statement,
                                    size_t index, uint8_t* out, size_t cap,
                                    size_t* copied) {
    int column = 0;
    int rc = column_index(statement, index, &column);
    if (rc != SQLITE_OK)
        return rc;
    if ((out == NULL && cap != 0) || copied == NULL)
        return SQLITE_MISUSE;
    const void* blob = sqlite3_column_blob(statement->stmt, column);
    int bytes = sqlite3_column_bytes(statement->stmt, column);
    size_t needed = bytes < 0 ? 0 : (size_t)bytes;
    if (blob == NULL && needed != 0)
        return SQLITE_NOMEM;
    size_t n = needed < cap ? needed : cap;
    if (n != 0)
        memcpy(out, blob, n);
    *copied = n;
    return SQLITE_OK;
}

int ciel_sqlite_ok_code(void) { return SQLITE_OK; }
int ciel_sqlite_row_code(void) { return SQLITE_ROW; }
int ciel_sqlite_done_code(void) { return SQLITE_DONE; }
int ciel_sqlite_type_integer(void) { return SQLITE_INTEGER; }
int ciel_sqlite_type_float(void) { return SQLITE_FLOAT; }
int ciel_sqlite_type_text(void) { return SQLITE_TEXT; }
int ciel_sqlite_type_blob(void) { return SQLITE_BLOB; }
int ciel_sqlite_type_null(void) { return SQLITE_NULL; }

CielConstSlice_char ciel_sqlite_error_message(int code) {
    const char* message = sqlite3_errstr(code);
    if (message == NULL)
        return CIEL_CONST_STR("sqlite error");
    return (CielConstSlice_char){.ptr = message, .len = strlen(message)};
}

CielConstSlice_char ciel_sqlite_libversion(void) {
    const char* version = sqlite3_libversion();
    return (CielConstSlice_char){.ptr = version, .len = strlen(version)};
}
