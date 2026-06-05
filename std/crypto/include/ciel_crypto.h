#ifndef CIEL_CRYPTO_H
#define CIEL_CRYPTO_H

#include "ciel_base.h"

#ifdef __cplusplus
extern "C" {
#endif

typedef struct CielCryptoRng CielCryptoRng;
typedef struct CielCryptoHash CielCryptoHash;
typedef struct CielCryptoMac CielCryptoMac;

CielConstSlice_char ciel_crypto_error_message(int32_t code);
int32_t ciel_crypto_random_bytes(uint8_t* out, size_t out_len);
int32_t ciel_crypto_system_rng(CielCryptoRng** out);
int32_t ciel_crypto_rng_random_bytes(CielCryptoRng* rng, uint8_t* out,
                                     size_t out_len);
int32_t ciel_crypto_hash_once(const char* algorithm, size_t algorithm_len,
                              const uint8_t* data, size_t data_len,
                              uint8_t* out, size_t out_len, size_t* written);
int32_t ciel_crypto_hash_new(const char* algorithm, size_t algorithm_len,
                             CielCryptoHash** out);
int32_t ciel_crypto_hash_update(CielCryptoHash* hash, const uint8_t* data,
                                size_t data_len);
int32_t ciel_crypto_hash_finish(CielCryptoHash* hash, uint8_t* out,
                                size_t out_len, size_t* written);
int32_t ciel_crypto_hash_clear(CielCryptoHash* hash);
int32_t ciel_crypto_mac_once(const char* algorithm, size_t algorithm_len,
                             const uint8_t* key, size_t key_len,
                             const uint8_t* data, size_t data_len, uint8_t* out,
                             size_t out_len, size_t* written);
int32_t ciel_crypto_mac_new(const char* algorithm, size_t algorithm_len,
                            const uint8_t* key, size_t key_len,
                            CielCryptoMac** out);
int32_t ciel_crypto_mac_update(CielCryptoMac* mac, const uint8_t* data,
                               size_t data_len);
int32_t ciel_crypto_mac_finish(CielCryptoMac* mac, uint8_t* out, size_t out_len,
                               size_t* written);
int32_t ciel_crypto_mac_clear(CielCryptoMac* mac);
bool ciel_crypto_constant_time_eq(const uint8_t* left, size_t left_len,
                                  const uint8_t* right, size_t right_len);

#ifdef __cplusplus
}
#endif

#endif
