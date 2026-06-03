#include "ciel_core.h"
#include "ciel_crypto.h"
#include "ciel_gc.h"

#include <botan/ffi.h>
#include <errno.h>
#include <string.h>

#define CIEL_CRYPTO_MAX_ALGORITHM_LEN 128
#define CIEL_CRYPTO_MIN_MAC_KEY_LEN 16

CielConstSlice_char ciel_crypto_error_message(int32_t code) {
    const char *message = NULL;
    if (code == BOTAN_FFI_ERROR_EXCEPTION_THROWN)
        message = botan_error_last_exception_message();
    if (message == NULL || message[0] == '\0')
        message = botan_error_description(code);
    if (message == NULL || message[0] == '\0')
        message = "Unknown Botan error";
    size_t len = strlen(message);
    char *copy = ciel_cstr_from_slice(message, len);
    return (CielConstSlice_char){.ptr = copy, .len = len};
}

struct CielCryptoRng {
    botan_rng_t rng;
};

struct CielCryptoHash {
    botan_hash_t hash;
};

struct CielCryptoMac {
    botan_mac_t mac;
};

static int32_t ciel_crypto_check_input(const void *ptr, size_t len) {
    return (ptr == NULL && len > 0) ? EINVAL : 0;
}

static int32_t ciel_crypto_check_output(const void *ptr, size_t len) {
    return (ptr == NULL && len > 0) ? EINVAL : 0;
}

static const uint8_t *ciel_crypto_input_ptr(const uint8_t *ptr, size_t len) {
    static const uint8_t empty = 0;
    return len == 0 && ptr == NULL ? &empty : ptr;
}

static int32_t ciel_crypto_algorithm_cstr(const char *algorithm,
                                          size_t algorithm_len, char **out) {
    if (out == NULL)
        return EINVAL;
    *out = NULL;
    if (algorithm == NULL || algorithm_len == 0 ||
        algorithm_len > CIEL_CRYPTO_MAX_ALGORITHM_LEN)
        return EINVAL;
    for (size_t i = 0; i < algorithm_len; i++) {
        if (algorithm[i] == '\0')
            return EINVAL;
    }
    *out = ciel_cstr_from_slice(algorithm, algorithm_len);
    return 0;
}

static void ciel_crypto_rng_finalizer(void *obj, void *client_data) {
    (void)client_data;
    CielCryptoRng *ctx = (CielCryptoRng *)obj;
    if (ctx == NULL)
        return;
    if (ctx->rng != NULL) {
        botan_rng_destroy(ctx->rng);
        ctx->rng = NULL;
    }
}

static void ciel_crypto_hash_finalizer(void *obj, void *client_data) {
    (void)client_data;
    CielCryptoHash *ctx = (CielCryptoHash *)obj;
    if (ctx != NULL && ctx->hash != NULL) {
        botan_hash_destroy(ctx->hash);
        ctx->hash = NULL;
    }
}

static void ciel_crypto_mac_finalizer(void *obj, void *client_data) {
    (void)client_data;
    CielCryptoMac *ctx = (CielCryptoMac *)obj;
    if (ctx != NULL && ctx->mac != NULL) {
        botan_mac_destroy(ctx->mac);
        ctx->mac = NULL;
    }
}

int32_t ciel_crypto_random_bytes(uint8_t *out, size_t out_len) {
    int32_t check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;
    if (out_len == 0)
        return 0;
    return botan_system_rng_get(out, out_len);
}

int32_t ciel_crypto_system_rng(CielCryptoRng **out) {
    if (out == NULL)
        return EINVAL;
    *out = NULL;
    botan_rng_t rng = NULL;
    int rc = botan_rng_init(&rng, "system");
    if (rc != 0)
        return rc;

    CielCryptoRng *ctx = (CielCryptoRng *)ciel_alloc(sizeof(CielCryptoRng));
    if (ctx == NULL) {
        botan_rng_destroy(rng);
        return ENOMEM;
    }
    ctx->rng = rng;
    ciel_register_finalizer(ctx, ciel_crypto_rng_finalizer, NULL);
    *out = ctx;
    return 0;
}

int32_t ciel_crypto_rng_random_bytes(CielCryptoRng *rng, uint8_t *out,
                                     size_t out_len) {
    if (rng == NULL || rng->rng == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;
    if (out_len == 0)
        return 0;

    return botan_rng_get(rng->rng, out, out_len);
}

int32_t ciel_crypto_hash_once(const char *algorithm, size_t algorithm_len,
                              const uint8_t *data, size_t data_len,
                              uint8_t *out, size_t out_len, size_t *written) {
    if (written == NULL)
        return EINVAL;
    *written = 0;
    char *algorithm_name = NULL;
    int32_t algorithm_check =
        ciel_crypto_algorithm_cstr(algorithm, algorithm_len, &algorithm_name);
    if (algorithm_check != 0)
        return algorithm_check;
    int32_t check = ciel_crypto_check_input(data, data_len);
    if (check != 0)
        return check;
    check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;

    botan_hash_t hash = NULL;
    int rc = botan_hash_init(&hash, algorithm_name, 0);
    if (rc != 0)
        return rc;

    size_t needed = 0;
    rc = botan_hash_output_length(hash, &needed);
    if (rc != 0) {
        botan_hash_destroy(hash);
        return rc;
    }
    if (out_len < needed) {
        *written = needed;
        botan_hash_destroy(hash);
        return ENOBUFS;
    }
    if (data_len > 0) {
        rc = botan_hash_update(hash, ciel_crypto_input_ptr(data, data_len),
                               data_len);
        if (rc != 0) {
            botan_hash_destroy(hash);
            return rc;
        }
    }
    rc = botan_hash_final(hash, out);
    botan_hash_destroy(hash);
    if (rc != 0)
        return rc;
    *written = needed;
    return 0;
}

int32_t ciel_crypto_hash_new(const char *algorithm, size_t algorithm_len,
                             CielCryptoHash **out) {
    if (out == NULL)
        return EINVAL;
    *out = NULL;
    char *algorithm_name = NULL;
    int32_t algorithm_check =
        ciel_crypto_algorithm_cstr(algorithm, algorithm_len, &algorithm_name);
    if (algorithm_check != 0)
        return algorithm_check;
    botan_hash_t hash = NULL;
    int rc = botan_hash_init(&hash, algorithm_name, 0);
    if (rc != 0)
        return rc;

    CielCryptoHash *ctx = (CielCryptoHash *)ciel_alloc(sizeof(CielCryptoHash));
    if (ctx == NULL) {
        botan_hash_destroy(hash);
        return ENOMEM;
    }
    ctx->hash = hash;
    ciel_register_finalizer(ctx, ciel_crypto_hash_finalizer, NULL);
    *out = ctx;
    return 0;
}

int32_t ciel_crypto_hash_update(CielCryptoHash *hash, const uint8_t *data,
                                size_t data_len) {
    if (hash == NULL || hash->hash == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_input(data, data_len);
    if (check != 0)
        return check;
    if (data_len == 0)
        return 0;
    return botan_hash_update(hash->hash, ciel_crypto_input_ptr(data, data_len),
                             data_len);
}

int32_t ciel_crypto_hash_finish(CielCryptoHash *hash, uint8_t *out,
                                size_t out_len, size_t *written) {
    if (written == NULL)
        return EINVAL;
    *written = 0;
    if (hash == NULL || hash->hash == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;

    size_t needed = 0;
    int rc = botan_hash_output_length(hash->hash, &needed);
    if (rc != 0)
        return rc;
    if (out_len < needed) {
        *written = needed;
        return ENOBUFS;
    }
    rc = botan_hash_final(hash->hash, out);
    if (rc != 0)
        return rc;
    *written = needed;
    return 0;
}

int32_t ciel_crypto_hash_clear(CielCryptoHash *hash) {
    if (hash == NULL || hash->hash == NULL)
        return EINVAL;
    botan_hash_t raw = hash->hash;
    hash->hash = NULL;
    return botan_hash_destroy(raw);
}

int32_t ciel_crypto_mac_once(const char *algorithm, size_t algorithm_len,
                             const uint8_t *key, size_t key_len,
                             const uint8_t *data, size_t data_len, uint8_t *out,
                             size_t out_len, size_t *written) {
    if (written == NULL)
        return EINVAL;
    *written = 0;
    char *algorithm_name = NULL;
    int32_t algorithm_check =
        ciel_crypto_algorithm_cstr(algorithm, algorithm_len, &algorithm_name);
    if (algorithm_check != 0)
        return algorithm_check;
    if (key_len < CIEL_CRYPTO_MIN_MAC_KEY_LEN)
        return EINVAL;
    int32_t check = ciel_crypto_check_input(key, key_len);
    if (check != 0)
        return check;
    check = ciel_crypto_check_input(data, data_len);
    if (check != 0)
        return check;
    check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;

    botan_mac_t mac = NULL;
    int rc = botan_mac_init(&mac, algorithm_name, 0);
    if (rc != 0)
        return rc;
    rc = botan_mac_set_key(mac, ciel_crypto_input_ptr(key, key_len), key_len);
    if (rc != 0) {
        botan_mac_destroy(mac);
        return rc;
    }

    size_t needed = 0;
    rc = botan_mac_output_length(mac, &needed);
    if (rc != 0) {
        botan_mac_destroy(mac);
        return rc;
    }
    if (out_len < needed) {
        *written = needed;
        botan_mac_destroy(mac);
        return ENOBUFS;
    }
    if (data_len > 0) {
        rc = botan_mac_update(mac, ciel_crypto_input_ptr(data, data_len),
                              data_len);
        if (rc != 0) {
            botan_mac_destroy(mac);
            return rc;
        }
    }
    rc = botan_mac_final(mac, out);
    botan_mac_destroy(mac);
    if (rc != 0)
        return rc;
    *written = needed;
    return 0;
}

int32_t ciel_crypto_mac_new(const char *algorithm, size_t algorithm_len,
                            const uint8_t *key, size_t key_len,
                            CielCryptoMac **out) {
    if (out == NULL)
        return EINVAL;
    *out = NULL;
    char *algorithm_name = NULL;
    int32_t algorithm_check =
        ciel_crypto_algorithm_cstr(algorithm, algorithm_len, &algorithm_name);
    if (algorithm_check != 0)
        return algorithm_check;
    if (key_len < CIEL_CRYPTO_MIN_MAC_KEY_LEN)
        return EINVAL;
    int32_t check = ciel_crypto_check_input(key, key_len);
    if (check != 0)
        return check;

    botan_mac_t mac = NULL;
    int rc = botan_mac_init(&mac, algorithm_name, 0);
    if (rc != 0)
        return rc;
    rc = botan_mac_set_key(mac, ciel_crypto_input_ptr(key, key_len), key_len);
    if (rc != 0) {
        botan_mac_destroy(mac);
        return rc;
    }

    CielCryptoMac *ctx = (CielCryptoMac *)ciel_alloc(sizeof(CielCryptoMac));
    if (ctx == NULL) {
        botan_mac_destroy(mac);
        return ENOMEM;
    }
    ctx->mac = mac;
    ciel_register_finalizer(ctx, ciel_crypto_mac_finalizer, NULL);
    *out = ctx;
    return 0;
}

int32_t ciel_crypto_mac_update(CielCryptoMac *mac, const uint8_t *data,
                               size_t data_len) {
    if (mac == NULL || mac->mac == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_input(data, data_len);
    if (check != 0)
        return check;
    if (data_len == 0)
        return 0;
    return botan_mac_update(mac->mac, ciel_crypto_input_ptr(data, data_len),
                            data_len);
}

int32_t ciel_crypto_mac_finish(CielCryptoMac *mac, uint8_t *out, size_t out_len,
                               size_t *written) {
    if (written == NULL)
        return EINVAL;
    *written = 0;
    if (mac == NULL || mac->mac == NULL)
        return EINVAL;
    int32_t check = ciel_crypto_check_output(out, out_len);
    if (check != 0)
        return check;

    size_t needed = 0;
    int rc = botan_mac_output_length(mac->mac, &needed);
    if (rc != 0)
        return rc;
    if (out_len < needed) {
        *written = needed;
        return ENOBUFS;
    }
    rc = botan_mac_final(mac->mac, out);
    if (rc != 0)
        return rc;
    *written = needed;
    return 0;
}

int32_t ciel_crypto_mac_clear(CielCryptoMac *mac) {
    if (mac == NULL || mac->mac == NULL)
        return EINVAL;
    botan_mac_t raw = mac->mac;
    mac->mac = NULL;
    return botan_mac_destroy(raw);
}

bool ciel_crypto_constant_time_eq(const uint8_t *left, size_t left_len,
                                  const uint8_t *right, size_t right_len) {
    if (left_len != right_len)
        return false;
    if (left_len == 0)
        return true;
    if (left == NULL || right == NULL)
        return false;
    return botan_constant_time_compare(left, right, left_len) == 0;
}
