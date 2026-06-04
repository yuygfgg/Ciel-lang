#ifndef CIEL_SQLITE_WRAP_H
#define CIEL_SQLITE_WRAP_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielSqliteConnection CielSqliteConnection;
typedef struct CielSqliteStatement CielSqliteStatement;

int ciel_sqlite_open(const char* path, size_t path_len, int flags,
                     CielSqliteConnection** out);
int ciel_sqlite_open_memory(CielSqliteConnection** out);
int ciel_sqlite_close(CielSqliteConnection* connection);
int ciel_sqlite_exec(CielSqliteConnection* connection, const char* sql,
                     size_t sql_len);
int ciel_sqlite_busy_timeout(CielSqliteConnection* connection, int milliseconds);
void ciel_sqlite_interrupt(CielSqliteConnection* connection);
int ciel_sqlite_last_insert_rowid(CielSqliteConnection* connection,
                                  int64_t* out);
int ciel_sqlite_changes(CielSqliteConnection* connection, int64_t* out);
int ciel_sqlite_total_changes(CielSqliteConnection* connection, int64_t* out);

int ciel_sqlite_prepare(CielSqliteConnection* connection, const char* sql,
                        size_t sql_len, CielSqliteStatement** out);
int ciel_sqlite_finalize(CielSqliteStatement* statement);
int ciel_sqlite_reset(CielSqliteStatement* statement);
int ciel_sqlite_clear_bindings(CielSqliteStatement* statement);
int ciel_sqlite_step(CielSqliteStatement* statement);
int ciel_sqlite_bind_parameter_count(CielSqliteStatement* statement,
                                     size_t* out);
int ciel_sqlite_bind_parameter_index(CielSqliteStatement* statement,
                                     const char* name, size_t name_len,
                                     size_t* out);

int ciel_sqlite_bind_null(CielSqliteStatement* statement, size_t index);
int ciel_sqlite_bind_i64(CielSqliteStatement* statement, size_t index,
                         int64_t value);
int ciel_sqlite_bind_f64(CielSqliteStatement* statement, size_t index,
                         double value);
int ciel_sqlite_bind_text(CielSqliteStatement* statement, size_t index,
                          const char* text, size_t text_len);
int ciel_sqlite_bind_blob(CielSqliteStatement* statement, size_t index,
                          const uint8_t* data, size_t data_len);

int ciel_sqlite_column_count(CielSqliteStatement* statement, size_t* out);
int ciel_sqlite_column_type(CielSqliteStatement* statement, size_t index,
                            int* out);
int ciel_sqlite_column_i64(CielSqliteStatement* statement, size_t index,
                           int64_t* out);
int ciel_sqlite_column_f64(CielSqliteStatement* statement, size_t index,
                           double* out);
int ciel_sqlite_column_name_len(CielSqliteStatement* statement, size_t index,
                                size_t* out);
int ciel_sqlite_column_name_copy_to(CielSqliteStatement* statement,
                                    size_t index, char* out, size_t cap,
                                    size_t* copied);
int ciel_sqlite_column_text_len(CielSqliteStatement* statement, size_t index,
                                size_t* out);
int ciel_sqlite_column_text_copy_to(CielSqliteStatement* statement,
                                    size_t index, char* out, size_t cap,
                                    size_t* copied);
int ciel_sqlite_column_blob_len(CielSqliteStatement* statement, size_t index,
                                size_t* out);
int ciel_sqlite_column_blob_copy_to(CielSqliteStatement* statement,
                                    size_t index, uint8_t* out, size_t cap,
                                    size_t* copied);

int ciel_sqlite_ok_code(void);
int ciel_sqlite_row_code(void);
int ciel_sqlite_done_code(void);
int ciel_sqlite_type_integer(void);
int ciel_sqlite_type_float(void);
int ciel_sqlite_type_text(void);
int ciel_sqlite_type_blob(void);
int ciel_sqlite_type_null(void);
CielConstSlice_char ciel_sqlite_error_message(int code);
CielConstSlice_char ciel_sqlite_libversion(void);

#ifdef __cplusplus
}
#endif

#endif
