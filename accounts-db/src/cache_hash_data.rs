//! Cached data for hashing accounts
#[cfg(test)]
use crate::pubkey_bins::PubkeyBinCalculator24;
use {
    crate::{accounts_hash::CalculateHashIntermediate, cache_hash_data_stats::CacheHashDataStats},
    bytemuck::{Pod, Zeroable},
    memmap2::MmapMut,
    solana_measure::measure::Measure,
    std::io::BufWriter,
    std::{
        collections::HashSet,
        fs::{self, remove_file, File, OpenOptions},
        io::Write,
        path::{Path, PathBuf},
        sync::{atomic::Ordering, Arc, Mutex},
    },
};

pub type EntryType = CalculateHashIntermediate;
pub type SavedType = Vec<Vec<EntryType>>;
pub type SavedTypeSlice = [Vec<EntryType>];

#[derive(Pod, Zeroable, Copy, Clone)]
#[repr(C)]
pub struct Header {
    count: usize,
}

/// cache hash data file to be mmapped later
pub(crate) struct CacheHashDataFileReference {
    file: File,
    file_len: u64,
    path: PathBuf,
    stats: Arc<CacheHashDataStats>,
}

/// mmapped cache hash data file
pub(crate) struct CacheHashDataFile {
    cell_size: u64,
    mmap: MmapMut,
    capacity: u64,
}

impl CacheHashDataFileReference {
    /// convert the open file refrence to a mmapped file that can be returned as a slice
    pub(crate) fn map(&self) -> Result<CacheHashDataFile, std::io::Error> {
        let file_len = self.file_len;
        let mut m1 = Measure::start("read_file");
        let mmap = CacheHashDataFileReference::load_map(&self.file)?;
        m1.stop();
        self.stats.read_us.fetch_add(m1.as_us(), Ordering::Relaxed);
        let header_size = std::mem::size_of::<Header>() as u64;
        if file_len < header_size {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }

        let cell_size = std::mem::size_of::<EntryType>() as u64;
        unsafe {
            assert_eq!(
                mmap.align_to::<EntryType>().0.len(),
                0,
                "mmap is not aligned"
            );
        }
        assert_eq!((cell_size as usize) % std::mem::size_of::<u64>(), 0);
        let mut cache_file = CacheHashDataFile {
            mmap,
            cell_size,
            capacity: 0,
        };
        let header = cache_file.get_header_mut();
        let entries = header.count;

        let capacity = cell_size * (entries as u64) + header_size;
        if file_len < capacity {
            return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof));
        }
        cache_file.capacity = capacity;
        assert_eq!(
            capacity, file_len,
            "expected: {capacity}, len on disk: {file_len} {}, entries: {entries}, cell_size: {cell_size}", self.path.display(),
        );

        self.stats
            .total_entries
            .fetch_add(entries, Ordering::Relaxed);
        self.stats
            .cache_file_size
            .fetch_add(capacity as usize, Ordering::Relaxed);

        self.stats.loaded_from_cache.fetch_add(1, Ordering::Relaxed);
        self.stats
            .entries_loaded_from_cache
            .fetch_add(entries, Ordering::Relaxed);
        Ok(cache_file)
    }

    fn load_map(file: &File) -> Result<MmapMut, std::io::Error> {
        Ok(unsafe { MmapMut::map_mut(file).unwrap() })
    }
}

impl CacheHashDataFile {
    /// return a slice of a reference to all the cache hash data from the mmapped file
    pub fn get_cache_hash_data(&self) -> &[EntryType] {
        self.get_slice(0)
    }

    #[cfg(test)]
    /// Populate 'accumulator' from entire contents of the cache file.
    pub fn load_all(
        &self,
        accumulator: &mut SavedType,
        start_bin_index: usize,
        bin_calculator: &PubkeyBinCalculator24,
    ) {
        let mut m2 = Measure::start("decode");
        let slices = self.get_cache_hash_data();
        for d in slices {
            let mut pubkey_to_bin_index = bin_calculator.bin_from_pubkey(&d.pubkey);
            assert!(
                pubkey_to_bin_index >= start_bin_index,
                "{pubkey_to_bin_index}, {start_bin_index}"
            ); // this would indicate we put a pubkey in too high of a bin
            pubkey_to_bin_index -= start_bin_index;
            accumulator[pubkey_to_bin_index].push(d.clone()); // may want to avoid clone here
        }

        m2.stop();
    }

    /// get '&[EntryType]' from cache file [ix..]
    fn get_slice(&self, ix: u64) -> &[EntryType] {
        let start = self.get_element_offset_byte(ix);
        let item_slice: &[u8] = &self.mmap[start..];
        let remaining_elements = item_slice.len() / std::mem::size_of::<EntryType>();
        unsafe {
            let item = item_slice.as_ptr() as *const EntryType;
            std::slice::from_raw_parts(item, remaining_elements)
        }
    }

    /// return byte offset of entry 'ix' into a slice which contains a header and at least ix elements
    fn get_element_offset_byte(&self, ix: u64) -> usize {
        let start = (ix * self.cell_size) as usize + std::mem::size_of::<Header>();
        debug_assert_eq!(start % std::mem::align_of::<EntryType>(), 0);
        start
    }

    fn get_header_mut(&mut self) -> &mut Header {
        let start = 0_usize;
        let end = start + std::mem::size_of::<Header>();
        let item_slice: &[u8] = &self.mmap[start..end];
        unsafe {
            let item = item_slice.as_ptr() as *mut Header;
            &mut *item
        }
    }
}

pub type PreExistingCacheFiles = HashSet<PathBuf>;
pub struct CacheHashData {
    cache_dir: PathBuf,
    pre_existing_cache_files: Arc<Mutex<PreExistingCacheFiles>>,
    pub stats: Arc<CacheHashDataStats>,
}

impl Drop for CacheHashData {
    fn drop(&mut self) {
        self.delete_old_cache_files();
        self.stats.report();
    }
}

impl CacheHashData {
    pub fn new(cache_dir: PathBuf) -> CacheHashData {
        std::fs::create_dir_all(&cache_dir).unwrap_or_else(|err| {
            panic!("error creating cache dir {}: {err}", cache_dir.display())
        });

        let result = CacheHashData {
            cache_dir,
            pre_existing_cache_files: Arc::new(Mutex::new(PreExistingCacheFiles::default())),
            stats: Arc::default(),
        };

        result.get_cache_files();
        result
    }
    fn delete_old_cache_files(&self) {
        let pre_existing_cache_files = self.pre_existing_cache_files.lock().unwrap();
        if !pre_existing_cache_files.is_empty() {
            self.stats
                .unused_cache_files
                .fetch_add(pre_existing_cache_files.len(), Ordering::Relaxed);
            for file_name in pre_existing_cache_files.iter() {
                let result = self.cache_dir.join(file_name);
                let _ = fs::remove_file(result);
            }
        }
    }
    fn get_cache_files(&self) {
        if self.cache_dir.is_dir() {
            let dir = fs::read_dir(&self.cache_dir);
            if let Ok(dir) = dir {
                let mut pre_existing = self.pre_existing_cache_files.lock().unwrap();
                for entry in dir.flatten() {
                    if let Some(name) = entry.path().file_name() {
                        pre_existing.insert(PathBuf::from(name));
                    }
                }
                self.stats
                    .cache_file_count
                    .fetch_add(pre_existing.len(), Ordering::Relaxed);
            }
        }
    }

    #[cfg(test)]
    /// load from 'file_name' into 'accumulator'
    pub(crate) fn load(
        &self,
        file_name: impl AsRef<Path>,
        accumulator: &mut SavedType,
        start_bin_index: usize,
        bin_calculator: &PubkeyBinCalculator24,
    ) -> Result<(), std::io::Error> {
        let mut m = Measure::start("overall");
        let cache_file = self.load_map(file_name)?;
        cache_file.load_all(accumulator, start_bin_index, bin_calculator);
        m.stop();
        self.stats.load_us.fetch_add(m.as_us(), Ordering::Relaxed);
        Ok(())
    }

    /// open a cache hash file, but don't map it.
    /// This allows callers to know a file exists, but preserves the # mmapped files.
    pub(crate) fn get_file_reference_to_map_later(
        &self,
        file_name: impl AsRef<Path>,
    ) -> Result<CacheHashDataFileReference, std::io::Error> {
        let path = self.cache_dir.join(&file_name);
        let file_len = std::fs::metadata(&path)?.len();
        let mut m1 = Measure::start("read_file");

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(false)
            .open(&path)?;
        m1.stop();
        self.stats.read_us.fetch_add(m1.as_us(), Ordering::Relaxed);
        self.pre_existing_cache_file_will_be_used(file_name);

        Ok(CacheHashDataFileReference {
            file,
            file_len,
            path,
            stats: Arc::clone(&self.stats),
        })
    }

    #[cfg(test)]
    /// map 'file_name' into memory
    fn load_map(&self, file_name: impl AsRef<Path>) -> Result<CacheHashDataFile, std::io::Error> {
        let reference = self.get_file_reference_to_map_later(file_name)?;
        reference.map()
    }

    pub(crate) fn pre_existing_cache_file_will_be_used(&self, file_name: impl AsRef<Path>) {
        self.pre_existing_cache_files
            .lock()
            .unwrap()
            .remove(file_name.as_ref());
    }

    /// save 'data' to 'file_name'
    pub fn save(
        &self,
        file_name: impl AsRef<Path>,
        data: &SavedTypeSlice,
    ) -> Result<(), std::io::Error> {
        self.save_internal(file_name, data)
    }

    fn save_internal(
        &self,
        file_name: impl AsRef<Path>,
        data: &SavedTypeSlice,
    ) -> Result<(), std::io::Error> {
        let mut m = Measure::start("save");
        let cache_path = self.cache_dir.join(file_name);
        // overwrite any existing file at this path
        let _ignored = remove_file(&cache_path);
        let cell_size = std::mem::size_of::<EntryType>() as u64;
        let mut m1 = Measure::start("create save");
        let entries = data
            .iter()
            .map(|x: &Vec<EntryType>| x.len())
            .collect::<Vec<_>>();
        let entries = entries.iter().sum::<usize>();
        let capacity = cell_size * (entries as u64) + std::mem::size_of::<Header>() as u64;

        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(cache_path)?;
        let mut fw = BufWriter::new(file);

        let header = Header { count: entries };
        let header_slice = bytemuck::bytes_of(&header);

        fw.write_all(header_slice)?;
        m1.stop();
        self.stats
            .create_save_us
            .fetch_add(m1.as_us(), Ordering::Relaxed);

        self.stats
            .cache_file_size
            .fetch_add(capacity as usize, Ordering::Relaxed);
        self.stats
            .total_entries
            .fetch_add(entries, Ordering::Relaxed);

        let mut m2 = Measure::start("write_to_mmap");
        let mut i = 0;
        for x in data {
            let size = x.len() * std::mem::size_of::<EntryType>();
            let slice = unsafe { std::slice::from_raw_parts(x.as_ptr() as *const u8, size) };
            fw.write_all(slice)?;
            i += x.len();
        }
        assert_eq!(i, entries);
        m2.stop();
        self.stats
            .write_to_mmap_us
            .fetch_add(m2.as_us(), Ordering::Relaxed);
        m.stop();
        self.stats.save_us.fetch_add(m.as_us(), Ordering::Relaxed);
        self.stats.saved_to_cache.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
pub mod tests {
    use {super::*, rand::Rng};

    #[test]
    fn test_read_write() {
        // generate sample data
        // write to file
        // read
        // compare
        use tempfile::TempDir;
        let tmpdir = TempDir::new().unwrap();
        let cache_dir = tmpdir.path().to_path_buf();
        std::fs::create_dir_all(&cache_dir).unwrap();

        for bins in [1, 2, 4] {
            let bin_calculator = PubkeyBinCalculator24::new(bins);
            let num_points = 5;
            let (data, _total_points) = generate_test_data(num_points, bins, &bin_calculator);
            for passes in [1, 2] {
                let bins_per_pass = bins / passes;
                if bins_per_pass == 0 {
                    continue; // illegal test case
                }
                for pass in 0..passes {
                    for flatten_data in [true, false] {
                        let mut data_this_pass = if flatten_data {
                            vec![vec![], vec![]]
                        } else {
                            vec![]
                        };
                        let start_bin_this_pass = pass * bins_per_pass;
                        for bin in 0..bins_per_pass {
                            let mut this_bin_data = data[bin + start_bin_this_pass].clone();
                            if flatten_data {
                                data_this_pass[0].append(&mut this_bin_data);
                            } else {
                                data_this_pass.push(this_bin_data);
                            }
                        }
                        let cache = CacheHashData::new(cache_dir.clone());
                        let file_name = PathBuf::from("test");
                        cache.save(&file_name, &data_this_pass).unwrap();
                        cache.get_cache_files();
                        assert_eq!(
                            cache
                                .pre_existing_cache_files
                                .lock()
                                .unwrap()
                                .iter()
                                .collect::<Vec<_>>(),
                            vec![&file_name],
                        );
                        let mut accum = (0..bins_per_pass).map(|_| vec![]).collect();
                        cache
                            .load(&file_name, &mut accum, start_bin_this_pass, &bin_calculator)
                            .unwrap();
                        if flatten_data {
                            bin_data(
                                &mut data_this_pass,
                                &bin_calculator,
                                bins_per_pass,
                                start_bin_this_pass,
                            );
                        }
                        assert_eq!(
                            accum, data_this_pass,
                            "bins: {bins}, start_bin_this_pass: {start_bin_this_pass}, pass: {pass}, flatten: {flatten_data}, passes: {passes}"
                        );
                    }
                }
            }
        }
    }

    fn bin_data(
        data: &mut SavedType,
        bin_calculator: &PubkeyBinCalculator24,
        bins: usize,
        start_bin: usize,
    ) {
        let mut accum: SavedType = (0..bins).map(|_| vec![]).collect();
        data.drain(..).for_each(|mut x| {
            x.drain(..).for_each(|item| {
                let bin = bin_calculator.bin_from_pubkey(&item.pubkey);
                accum[bin - start_bin].push(item);
            })
        });
        *data = accum;
    }

    fn generate_test_data(
        count: usize,
        bins: usize,
        binner: &PubkeyBinCalculator24,
    ) -> (SavedType, usize) {
        let mut rng = rand::thread_rng();
        let mut ct = 0;
        (
            (0..bins)
                .map(|bin| {
                    let rnd = rng.gen::<u64>() % (bins as u64);
                    if rnd < count as u64 {
                        (0..std::cmp::max(1, count / bins))
                            .map(|_| {
                                ct += 1;
                                let mut pk;
                                loop {
                                    // expensive, but small numbers and for tests, so ok
                                    pk = solana_sdk::pubkey::new_rand();
                                    if binner.bin_from_pubkey(&pk) == bin {
                                        break;
                                    }
                                }

                                CalculateHashIntermediate::new(
                                    solana_sdk::hash::Hash::new_unique(),
                                    ct as u64,
                                    pk,
                                )
                            })
                            .collect::<Vec<_>>()
                    } else {
                        vec![]
                    }
                })
                .collect::<Vec<_>>(),
            ct,
        )
    }
}
