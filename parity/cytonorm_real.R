#!/usr/bin/env Rscript
# Real acquisitions and black-box R comparisons; never reads upstream implementation.
suppressPackageStartupMessages({library(CytoNorm); library(flowCore); library(jsonlite)})
stopifnot(as.character(packageVersion("CytoNorm")) == "2.0.12")
args <- commandArgs(TRUE)
binary <- normalizePath(if (length(args)) args[1] else "target/debug/cytiq", mustWork = TRUE)
out <- if (length(args) > 1) args[2] else "parity/.out/cytonorm-real"
dir.create(out, recursive = TRUE, showWarnings = FALSE)
out <- normalizePath(out)
for (dir in c("prepared", "normalized", "assignments", "r-clusters")) {
  dir.create(file.path(out, dir), showWarnings = FALSE)
}
run <- function(args) {
  status <- system2(binary, shQuote(args))
  if (status != 0) stop("cytiq failed: ", paste(args, collapse = " "))
}
read_frame <- function(path) read.FCS(path, transformation = FALSE, truncate_max_range = FALSE)
read_values <- function(path) exprs(read_frame(path))
hash <- function(path) digest::digest(file = path, algo = "sha256")
json <- function(value, path) write_json(value, path, auto_unbox = TRUE, pretty = TRUE,
                                        digits = 17, na = "null")

# Six unmodified 1,000-cell excerpts from FR-FCM-Z247, shipped by the pinned package.
expected <- c(
  Gates_PTLG021_Unstim_Control_1.fcs = "852c6c346168e4e39e7d990729d41aa6cd53d0794debeb73bc396292678f58b8",
  Gates_PTLG021_Unstim_Control_2.fcs = "53e2e57ef6a2c376ab43ce18b096e16d3043b1ea48c6a990b4fb2d08b6166bcd",
  Gates_PTLG028_Unstim_Control_1.fcs = "78b8d9afab4e79e72b6efe8c4f4e50e452ec82936481de1b7056b2f11d88dcb1",
  Gates_PTLG028_Unstim_Control_2.fcs = "d299ad706a5d643b2bc3ac4b49899a76b7cd2f6bc214a1fe6c1f4aada14b04f4",
  Gates_PTLG034_Unstim_Control_1.fcs = "413a942f6fd62a0743c65fd3b3cbe5f5952771bfefcfa898f6a5dfa54ca59d43",
  Gates_PTLG034_Unstim_Control_2.fcs = "2b7c0a98b1ac11f9198a1e04f6193383226c92e9ac396fc393a6bf36b9a185cd"
)
source <- system.file("extdata", package = "CytoNorm")
files <- file.path(source, names(expected))
stopifnot(all(file.exists(files)), identical(unname(vapply(files, hash, "")), unname(expected)))
ids <- sub(".fcs$", "", names(expected))
batches <- sub(".*(PTLG[0-9]+).*", "\\1", ids)
roles <- ifelse(grepl("_1$", ids), "train", "validation")
train <- which(roles == "train")
frames <- lapply(files, read_frame)
stopifnot(all(vapply(frames, function(ff) nrow(exprs(ff)) == 1000, TRUE)))
p <- Biobase::pData(parameters(frames[[1]]))
p$name <- as.character(p$name)
p$desc <- as.character(p$desc)
# The same 37 biological channels as the upstream README's control workflow.
selected <- c(48, 46, 43, 45, 20, 16, 21, 19, 22, 50, 47, 40, 44, 33, 17, 11,
              18, 51, 14, 23, 32, 10, 49, 27, 24, 31, 42, 37, 39, 34, 41, 26,
              30, 28, 29, 25, 35)
channels <- p$name[selected]
markers <- p$desc[selected]
stopifnot(!anyNA(markers), !anyDuplicated(markers))
panel_rows <- which(!p$name %in% c("Time", "Event_length"))
missing_labels <- is.na(p$desc) | !nzchar(p$desc)
p$desc[missing_labels] <- p$name[missing_labels]
panel <- list(id = "CytoNorm-FR-FCM-Z247", markers = lapply(panel_rows, function(i) {
  list(marker = p$desc[i], channel = p$name[i])
}))

# CyTOF's example has no spillover matrix. Explicit identity compensation admits
# it through cytiq's signal-state contract; this does NOT estimate CyTOF spillover.
identity <- diag(length(channels))
dimnames(identity) <- list(channels, channels)
matrix_path <- file.path(out, "identity-compensation.csv")
write.csv(identity, matrix_path, quote = FALSE)
transformed <- file.path(out, "prepared", paste0(ids, ".t.fcs"))
baseline <- list()
for (i in seq_along(files)) {
  ff <- frames[[i]]
  params <- Biobase::pData(parameters(ff))
  stopifnot(identical(as.character(params$name), p$name),
            identical(as.character(params$desc[selected]), markers),
            !any(c("SPILL", "$SPILL", "SPILLOVER", "$SPILLOVER") %in% names(keyword(ff))))
  params$desc[missing_labels] <- params$name[missing_labels]
  parameters(ff) <- Biobase::AnnotatedDataFrame(params)
  prepared <- file.path(out, "prepared", paste0(ids[i], ".fcs"))
  write.FCS(ff, prepared, what = "numeric", endian = "little")
  stopifnot(identical(unname(read_values(prepared)), unname(exprs(frames[[i]]))))
  comp <- file.path(out, "prepared", paste0(ids[i], ".comp.fcs"))
  run(c("compensate", "--input", prepared, "--output", comp, "--matrix", matrix_path,
        "--matrix-kind", "compensation", "--overwrite"))
  stopifnot(identical(unname(read_values(comp)), unname(exprs(frames[[i]]))))
  run(c("transform", "--input", comp, "--output", transformed[i], "--method", "arcsinh",
        "--cofactor", "5", "--channels", paste(channels, collapse = ","), "--overwrite"))
  baseline[[i]] <- read_values(transformed[i])
  stopifnot(max(abs(baseline[[i]][, channels] - asinh(exprs(frames[[i]])[, channels] / 5))) < 1e-6,
            identical(unname(baseline[[i]][, -selected]), unname(exprs(frames[[i]])[, -selected])))
}
manifest <- list(schema_version = 1, panel = panel, mode = "controls",
                 clustering_markers = markers, normalization_markers = markers,
                 target = list(kind = "mean"),
                 som = list(xdim = 5, ydim = 5, metaclusters = 5, scale = FALSE, seed = 1),
                 training_events = 3000, min_cluster_events = 100, sparse_policy = "identity",
                 samples = lapply(seq_along(files), function(i) {
                   list(id = ids[i], batch = batches[i], role = roles[i],
                        file = file.path("prepared", basename(transformed[i])))
                 }))
manifest_path <- file.path(out, "batches.json")
json(manifest, manifest_path)
model_dir <- file.path(out, "model")
run(c("cytonorm", "train", "--manifest", manifest_path, "--output", model_dir,
      "--threads", "1", "--debug-dir", file.path(out, "diagnostics"), "--overwrite"))
model <- fromJSON(file.path(model_dir, "model.json"))
qc <- fromJSON(file.path(model_dir, "training-qc.json"))
coefficient_path <- file.path(out, "rust-coefficients.json")
stopifnot(system2("parity/.env/py/bin/python", shQuote(c("parity/cytonorm_coefficients.py", model_dir, coefficient_path))) == 0)
coefficients <- fromJSON(coefficient_path, simplifyVector=FALSE)
coefficient_error <- 0
for (curve in coefficients) {
  x <- unlist(curve$x); y <- unlist(curve$y); actual <- unlist(curve$slope)
  expected_slopes <- splinefun(x,y,method="monoH.FC")(x,deriv=1)
  stopifnot(all(abs(actual-expected_slopes) <= 1e-10+1e-10*abs(expected_slopes)))
  coefficient_error <- max(coefficient_error,max(abs(actual-expected_slopes)))
}

# R's sequential adjacent corrections can leave a negative derivative.
# Certify independently, rather than silently calling such a curve monotone.
monotone_oracle <- function(x, y) {
  f <- suppressWarnings(splinefun(x,y,method="monoH.FC")); x <- sort(unique(x))
  d <- diff(f(x))/diff(x); m <- f(x,deriv=1)
  all(vapply(seq_along(d), function(i) {
    if (d[i] == 0) return(m[i] == 0 && m[i+1] == 0)
    a <- m[i]/d[i]; b <- m[i+1]/d[i]; A <- 3*(a+b-2); B <- 2*(3-2*a-b)
    t <- if(A > 0) -B/(2*A) else -1
    min(a,b,if(t > 0 && t < 1) a+t*(B+t*A) else Inf) >= -1e-12
  }, TRUE))
}

# Fit again without validation, with a different thread count. Exact persisted
# artifacts must match: validation data must never influence the learned model.
train_manifest <- manifest
train_manifest$samples <- manifest$samples[train]
train_manifest_path <- file.path(out, "training-only.json")
json(train_manifest, train_manifest_path)
train_only_dir <- file.path(out, "model-training-only")
run(c("cytonorm", "train", "--manifest", train_manifest_path, "--output", train_only_dir,
      "--threads", "2", "--overwrite"))
payloads <- c("model.json", "som.json", "markers.json", "splines.parquet")
stopifnot(all(vapply(payloads, function(name) {
  identical(hash(file.path(model_dir, name)), hash(file.path(train_only_dir, name)))
}, TRUE)))

labels <- normalized <- list()
for (i in seq_along(files)) {
  assigned <- file.path(out, "assignments", paste0(ids[i], ".csv"))
  run(c("flowsom", "apply", "--input", transformed[i], "--model", file.path(model_dir, "som.json"),
        "--output", assigned, "--overwrite"))
  labels[[i]] <- read.csv(assigned)$flowsom_metacluster
  dest <- file.path(out, "normalized", paste0(ids[i], ".norm.fcs"))
  run(c("cytonorm", "apply", "--input", transformed[i], "--model", model_dir,
        "--batch", batches[i], "--output", dest, "--overwrite"))
  normalized[[i]] <- read_values(dest)
  before <- read_frame(transformed[i])
  after <- read_frame(dest)
  stopifnot(identical(dim(normalized[[i]]), dim(baseline[[i]])),
            identical(colnames(normalized[[i]]), colnames(baseline[[i]])),
            all(is.finite(normalized[[i]])),
            identical(unname(normalized[[i]][, -selected]), unname(baseline[[i]][, -selected])),
            identical(keyword(before)[["CYTIQ_ACQUISITION"]], keyword(after)[["CYTIQ_ACQUISITION"]]),
            !is.null(keyword(after)[["CYTIQ_NORMALIZATION"]]))
}

# Hold Rust cluster assignments constant and independently fit the upstream
# splines. Determine eligible batches using R quantiles, rather than Rust QC.
probabilities <- model$probabilities
conditional_error <- target_error <- 0
compared_values <- identity_values <- 0L
unsafe_oracle_values <- unsafe_oracle_mappings <- 0L
worst_case <- NULL
worst_curve <- NULL
spline_errors <- list()
for (cluster in seq_len(manifest$som$metaclusters)) {
  cluster_files <- vapply(train, function(i) {
    path <- file.path(out, "r-clusters", paste0(ids[i], "-", cluster, ".fcs"))
    values <- baseline[[i]][labels[[i]] == cluster, channels, drop = FALSE]
    if (nrow(values)) write.FCS(flowFrame(values), path, what = "numeric", endian = "little")
    path
  }, "")
  for (j in seq_along(channels)) {
    values <- lapply(train, function(i) baseline[[i]][labels[[i]] == cluster, channels[j]])
    quantiles <- lapply(values, function(v) {
      if (length(v)) as.numeric(quantile(v, probabilities, type = 7)) else numeric()
    })
    eligible <- vapply(seq_along(values), function(b) {
      length(values[[b]]) >= manifest$min_cluster_events &&
        length(quantiles[[b]]) > 1 && diff(range(quantiles[[b]])) > 0
    }, TRUE)
    ref <- NULL
    safe <- eligible
    if (any(eligible)) {
      expected_goal <- Reduce(`+`, quantiles[eligible]) / sum(eligible)
      safe <- vapply(seq_along(eligible),function(b) eligible[b] && monotone_oracle(quantiles[[b]],expected_goal),TRUE)
      unsafe_oracle_mappings <- unsafe_oracle_mappings + sum(eligible & !safe)
      goal <- model$goals[model$goals$metacluster == cluster & model$goals$marker == markers[j], ]
      target_error <- max(target_error, max(abs(goal$quantiles[[1]] - expected_goal)))
      ref <- QuantileNorm.train(files = cluster_files[eligible], labels = batches[train][eligible],
                                channels = channels[j], transformList = NULL,
                                nQ = length(probabilities), quantileValues = probabilities,
                                goal = "mean", transformation = FALSE, truncate_max_range = FALSE)
    }
    for (i in seq_along(files)) {
      rows <- which(labels[[i]] == cluster)
      if (!length(rows)) next
      b <- match(batches[i], batches[train])
      original <- baseline[[i]][rows, channels[j]]
      if (safe[b]) {
        expected_values <- ref$splines[[batches[i]]][[channels[j]]](original)
        actual <- normalized[[i]][rows, channels[j]]
        errors <- abs(expected_values - actual)
        largest <- which.max(errors)
        outside <- original < min(quantiles[[b]]) | original > max(quantiles[[b]])
        spline_errors[[length(spline_errors) + 1L]] <- data.frame(sample = ids[i], role = roles[i],
          batch = batches[i], metacluster = cluster, marker = markers[j], events = length(rows),
          max_absolute_error = max(errors), max_interior_error = max(c(0, errors[!outside])),
          max_tail_error = max(c(0, errors[outside])))
        if (max(errors) > conditional_error) {
          worst_case <- list(sample = ids[i], role = roles[i], batch = batches[i],
            metacluster = cluster, marker = markers[j], event_index_one_based = rows[largest],
            input = original[largest], rust = actual[largest], R = expected_values[largest],
            outside_knots = outside[largest])
          worst_curve <- data.frame(input = original, rust = actual, R = expected_values)
        }
        conditional_error <- max(conditional_error, max(errors))
        compared_values <- compared_values + length(rows)
      } else {
        stopifnot(identical(original, normalized[[i]][rows, channels[j]]))
        if (eligible[b]) {
          reason <- qc$distributions$identity_reason[qc$distributions$batch == batches[i] &
            qc$distributions$metacluster == cluster & qc$distributions$marker == markers[j] & qc$distributions$role == "train"]
          stopifnot(length(reason)==1, !is.na(reason), grepl("unsafe spline",reason,fixed=TRUE))
          unsafe_oracle_values <- unsafe_oracle_values + length(rows)
        }
        identity_values <- identity_values + length(rows)
      }
    }
  }
}
cat("Conditional spline error:", conditional_error, "; target error:", target_error, "\n")
stopifnot(compared_values > 0)
spline_errors <- do.call(rbind, spline_errors)
write.csv(spline_errors, file.path(out, "spline-errors.csv"), row.names = FALSE)
if (!is.null(worst_curve)) {
  curve <- worst_curve[order(worst_curve$input), ]
  pdf(file.path(out, "spline-comparison.pdf"), width = 10, height = 5)
  par(mfrow = c(1, 2))
  plot(curve$input, curve$R, type = "l", col = "firebrick", lwd = 2,
       xlab = "Input, arcsinh(x/5)", ylab = "Normalized signal",
       ylim = range(curve$rust, curve$R), main = paste(worst_case$marker, worst_case$batch))
  lines(curve$input, curve$rust, col = "steelblue", lwd = 2)
  legend("topleft", c("R CytoNorm", "cytiq"), col = c("firebrick", "steelblue"), lty = 1, bty = "n")
  plot(curve$input, curve$rust - curve$R, type = "l", col = "steelblue",
       xlab = "Input, arcsinh(x/5)", ylab = "cytiq minus R", main = "Same cluster assignments")
  abline(h = 0, col = "grey50", lty = 2)
  dev.off()
}

# Independent stochastic clustering is descriptive: real populations do not
# justify the synthetic test's ARI >= .95 threshold.
upstream <- CytoNorm.train(files = transformed[train], labels = batches[train], channels = channels,
  transformList = NULL, FlowSOM.params = list(nCells = 3000, xdim = 5, ydim = 5,
  nClus = 5, scale = FALSE, colsToUse = channels),
  normParams = list(nQ = 99, quantileValues = probabilities), seed = 1, plot = FALSE,
  outputDir = file.path(out, "r-model"), recompute = TRUE,
  transformation = FALSE, truncate_max_range = FALSE)
ari <- vapply(seq_along(files), function(i) {
  fsom <- FlowSOM::NewData(upstream$fsom, transformed[i], transform = FALSE, scale = FALSE)
  mclust::adjustedRandIndex(labels[[i]], FlowSOM::GetMetaclusters(fsom))
}, 0.0)

# Pairwise batch distances within each donor, using the original assignments.
# This compares the held-out donor across batches, not against donor 1's target.
distances <- list()
for (role in c("train", "validation")) {
  for (pair in combn(which(roles == role), 2, simplify = FALSE)) {
    for (cluster in seq_len(manifest$som$metaclusters)) {
      a <- which(labels[[pair[1]]] == cluster)
      b <- which(labels[[pair[2]]] == cluster)
      if (!length(a) || !length(b)) next
      for (j in seq_along(channels)) {
        q <- function(values, i, rows) quantile(values[[i]][rows, channels[j]], probabilities, type = 7)
        distances[[length(distances) + 1L]] <- data.frame(role = role,
          batch_a = batches[pair[1]], batch_b = batches[pair[2]], metacluster = cluster,
          marker = markers[j], events_a = length(a), events_b = length(b),
          before = mean(abs(q(baseline, pair[1], a) - q(baseline, pair[2], b))),
          after = mean(abs(q(normalized, pair[1], a) - q(normalized, pair[2], b))))
      }
    }
  }
}
distances <- do.call(rbind, distances)
write.csv(distances, file.path(out, "batch-distances.csv"), row.names = FALSE)
summary <- lapply(c("train", "validation"), function(role) {
  d <- distances[distances$role == role, ]
  list(role = role, comparisons = nrow(d), mean_before = mean(d$before), mean_after = mean(d$after),
       reduction_fraction = 1 - mean(d$after) / mean(d$before), fraction_improved = mean(d$after < d$before))
})
training_distributions <- qc$distributions[qc$distributions$role == "train", ]
provenance <- fromJSON(file.path(model_dir, "provenance.json"))
metrics <- list(schema_version = 1, dataset = "CytoNorm extdata / FR-FCM-Z247",
  cytiq_version = provenance$version, cytiq_build = provenance$build,
  CytoNorm = as.character(packageVersion("CytoNorm")), events = 6000,
  train_events = 3000, validation_events = 3000, batches = unique(batches),
  normalization_markers = length(channels), settings = manifest[c("som", "min_cluster_events", "sparse_policy")],
  source_sha256 = as.list(expected), identity_compensation_preserved_values = TRUE,
  unchanged_channels_and_event_order_preserved = TRUE,
  validation_exclusion_and_thread_determinism = TRUE,
  conditional_max_absolute_error = conditional_error, target_max_absolute_error = target_error,
  coefficient_max_absolute_error = coefficient_error,
  unsafe_oracle_identity_mappings = unsafe_oracle_mappings,
  unsafe_oracle_identity_values = unsafe_oracle_values,
  conditional_max_interior_error = max(spline_errors$max_interior_error),
  conditional_max_tail_error = max(spline_errors$max_tail_error), worst_case = worst_case,
  passed = conditional_error <= 1e-9 && target_error <= 1e-12,
  compared_values = compared_values, identity_values = identity_values,
  identity_mappings = sum(!is.na(training_distributions$identity_reason)),
  total_mappings = nrow(training_distributions),
  full_pipeline_ARI = setNames(as.list(ari), ids),
  batch_distances_quantile_approx = summary, occupancy_cv = qc$occupancy_cv,
  limitations = c("Only the first 1000 real cells per file; not the full acquisitions.",
    "Non-monotone upstream monoH.FC curves are refused; explicit sparse_policy identity retains their original values.",
    "Explicit identity compensation is a harness adapter, not a CyTOF spillover estimate.",
    "Missing channel labels are filled from physical names in prepared copies only.",
    "Five metaclusters are pilot settings; the minimum remains the default 100 events.",
    "Sparse mappings explicitly retain original values; inspect their QC reasons.",
    "Distances are equal-weight means of within-cluster quantile-grid approximations.",
    "Real-data distances and stochastic ARI are descriptive, not biological acceptance criteria."))
json(metrics, file.path(out, "metrics.json"))
results <- Sys.getenv("CYTIQ_PARITY_RESULTS", unset="target/scientific/results")
dir.create(results, recursive=TRUE, showWarnings=FALSE)
json(metrics, file.path(results, "metrics_cytonorm_real.json"))
cat(toJSON(metrics[c("conditional_max_absolute_error", "identity_mappings", "total_mappings",
                     "batch_distances_quantile_approx")], auto_unbox = TRUE, pretty = TRUE), "\n")
cat("Real CytoNorm artifacts:", out, "\n")
if (!metrics$passed) stop("Real CytoNorm numerical comparison failed; see metrics.json and spline-errors.csv")
cat("Real CytoNorm checks passed.\n")
